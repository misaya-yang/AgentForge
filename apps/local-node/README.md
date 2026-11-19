# AI--Platfform Local Node

This package is the device-side capability broker described by
`deploy/runbooks/assistant-local-os-product-contract.md`. It follows the useful
parts of OpenClaw's paired-node/action-receipt model and Hermes Agent's
capability registry/doctor model, while keeping AI--Platfform's canonical
AgentLoop as the only orchestrator.

## OpenClaw and Hermes implementation mapping

This is an implementation-level mapping, not a claim that those products were
copied wholesale:

| Reference implementation | Pattern retained here | Unsafe/default behavior deliberately rejected |
|---|---|---|
| OpenClaw Gateway + paired nodes | A device identity is designed for OS-backed secure storage, pairing uses a one-time challenge, transport is outbound TLS or loopback only, and side effects use idempotency keys plus durable receipts | A node capability claim is not authority; there is no global gateway token, plaintext identity-key fallback, default host-wide workspace, public listener, or sandbox-off fallback |
| OpenClaw host file/exec tools | Local actions are distinct from server workspace tools; cwd, argv, target digest, device/run identity and approval are explicit | There is no ambient shell string, implicit HOME access, inherited secrets, automatic retry of unknown side effects, or pretend network isolation |
| Hermes tool registry + doctor | Capabilities report `ready / denied / needs_action / unsupported`; Computer Use is a stateful driver contract whose availability is checked before a lease | Missing approval, driver, permission probe or policy fails closed instead of silently enabling the action |
| Hermes file/CUA backends | Files are explicit tool calls; desktop control is behind a per-session observation/action driver and not treated as ordinary web automation | File reads are not advertised as whole-machine real-time awareness, and a function-tool transport is not advertised as native OpenAI Computer Use |

The resulting topology is therefore `Web Assistant -> canonical AgentLoop ->
ExecutionGateway/device channel -> Local Node capability broker`. This package
implements only the final device-side broker. It does not create a second
planner or model loop.

The implementation is the headless Rust `ai-platform-local-node` crate in
`rust/platform-native/crates/platform-local-node`. The retained Swift helper is
in `apps/local-node/native/macos`. It provides:

- loopback-only and outbound-only boundary validation;
- one-time pairing challenges and an injectable OS secure-credential-store seam;
- mandatory, independently injected verification of platform-signed canonical
  action envelopes before any durable dispatch or local side effect;
- explicit directory grants and descriptor-based path containment;
- local file list/read/search/watch plus atomic writes and rollback receipts;
- structured, allowlisted process execution with a clean environment;
- a hash-chained SQLite action ledger and idempotency enforcement;
- an explicitly injected macOS Accessibility/CoreGraphics helper backend and
  driver contract that fail closed unless exact app/session/window scope and
  OS permission probes are ready;
- a macOS trusted-local approval prompt/Keychain signing adapter for exact
  action receipts; it is not a device-identity or transport-credential store;
- a doctor report suitable for a future trusted native UI.
- an outbound-only HTTPS device transport seam that heartbeats capability truth,
  claims platform-signed canonical actions, validates expiry/nonces/device scope,
  handles signed cancel/emergency-stop commands, and uploads an ordered
  digest-only receipt outbox;
- a non-starting `local-node doctor` command that never starts networking,
  reads credentials, or exposes a server/listener.

It does **not** expose an HTTP listener, install a daemon, inherit shell
credentials or infer macOS Accessibility/Screen Recording permission. The native
headless CLI now explicitly composes Keychain-backed Ed25519 device identity,
pinned platform keys, an operator-selected TLS CA, granted filesystem roots and
an optional native approval helper. Computer Use remains a separate, gated path.
See [the current CLI/config instructions](../../rust/platform-native/crates/platform-local-node/README.md).

The concrete outbound adapter uses the exact HTTPS v2 endpoint
`/api/v1/local-nodes/control`, verifies certificate chains and hostnames, disables
redirects and POST retry, and verifies the platform's signed pairing response
against the challenge and device identity. An explicitly supplied CA is the only
trust root for that client. There is no insecure file-store or test-HMAC option
in the production CLI. WSS still requires a separate explicit adapter.

Read-only `local-node doctor --config /path/to/local-node.json` never opens
Keychain, pairs a device or starts networking. Pair/serve use the real native
secure store; non-macOS platforms without a secure adapter fail closed. For
write grants, serve requires the configured native approval helper and verifies
its separate exact-intent, one-use signature before any write. Platform approval
never substitutes for the local owner prompt.

`ActionContext` is data, not authority. Every executable action binds tenant,
user, device, session, run, agent identity and version, envelope version, tool name, operation,
capability lease, resource
references and their digest, arguments/target/policy digests, nonce, issue/expiry
times, and any exact approval receipt into one canonical signed payload. The
verifier is injected independently into `ActionLedger`; a missing verifier,
unknown key, invalid signature, field mutation, or approval attached after
signing fails closed. The companion state machine matches the platform API:
`policy_check -> awaiting_approval -> dispatched -> running -> observed ->`
one terminal state. `observed` is optional for non-UI actions; dispatched
side effects that lose transport truth become terminal `unknown` and are never
automatically replayed.

Platform approval and trusted-local approval are separate authorities. A
platform-signed action may request an operation, but write, rollback, process,
and app-control side effects additionally require an independently device-local
signed receipt bound to device, action, arguments, target, policy, expiry, and a
durably one-use nonce. The verifier is injected into the ledger from the trusted
native composition root. Missing verifier, forged receipt, scope mutation, or
nonce replay fails closed. The macOS helper includes an explicitly constructed
approval prompt and Keychain-backed signer, but the standalone CLI does not
build that native composition; its trusted-local-approval doctor truth remains
`unavailable` until the packaged companion injects it.

A side-effect action is therefore a two-step signed composition: the platform
first registers an unsigned-for-local-approval proposal; after the device-local
receipt is returned over the authenticated device channel, the platform must
attach that exact receipt and freshly sign the complete final envelope. Adding a
receipt to an already platform-signed envelope is rejected.

Computer provider primitives are normalized into platform capabilities before
they reach the driver. Input injection (`click`, `type_text`, `scroll`, and the
other supported primitives) requires the signed `app.control` capability;
legacy/ad-hoc `computer.<primitive>`, `app.observe`, and `screen.observe` cannot
authorize control. The signed canonical operation is also `app.control`; the
specific primitive remains covered by `arguments_digest`. Observation and screen sharing remain distinct read-only
capabilities for the future transport/UI integration.

The canonical server tool currently proposes a bounded action batch and binds
the app grant/window, while `ComputerController` consumes one primitive at a
time under its own session lease and exact app/window references. No production
router currently proves that translation. App control must therefore remain
capability-gated and unavailable through the outbound canonical path until that
single-primitive lease mapping is implemented and accepted; the presence of a
native backend is not end-to-end authorization.

Rollback is the signed operation `file.rollback` beneath the canonical
`file.write` capability; it is not a separately grantable platform capability.
The signed tool name and operation prevent a generic write envelope from being
reinterpreted as rollback (or vice versa).

The host process runner cannot prove network isolation. Canonical
control-plane requests use `network_policy="deny"` or
`network_policy="allow_granted_domains"`; both remain unavailable until an
explicit network-enforcing process backend is injected. The test-only host path
can exercise argv, sanitized-environment, cancellation, and ledger behavior via
an explicit local `inherit` policy, but that mode is not exposed by the
canonical tool schema and is not production process readiness.

Multi-file analysis is deliberately grant-first and bounded. The caller supplies
an opaque directory grant plus an explicit ordered tuple of relative paths;
`analyze_files` returns exact local bytes and SHA-256 values together with
path/line/column/hash-bound grep citations. It never expands to the user's home
directory, and an unsafe path, missing capability, expired grant, or revoked
grant fails closed. Watch events contain relative paths, hashes, sizes, and event
metadata, but never file bodies.

The tmp-only local acceptance receipt is E2 only for real file and polling
behavior. It does not prove a native directory picker, durable grant persistence,
provider analysis, user-directory access, or browser/desktop Computer Use.

Run focused native tests from the repository root:

```bash
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-local-node
```

The following acceptance commands are explicit operational runs, separate from
unit tests. The first creates fresh temporary files, watcher events and real
allowlisted processes. It emits a JSON receipt and retains the original
test-only signer and no-real-WSS evidence boundaries.

```bash
cargo run --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-local-node --bin local_live_acceptance
```

On macOS, the second compiles the retained Swift helper with Xcode command-line
tools, launches Calculator, requests a native exact-intent approval, clicks the
observed 7 button and verifies the Accessibility readback. Before invoking it,
review and grant the appropriate Accessibility and Screen Recording permissions
in System Settings → Privacy & Security for the actual execution host. Permission
denial remains a failed acceptance result; the runner does not bypass it. The
run can access its dedicated approval Keychain item and writes a private
screenshot beneath the explicitly selected report directory.

```bash
cargo run --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-local-node --bin macos_computer_live_acceptance -- \
  --report-dir /absolute/private/report-directory
```

Neither command is evidence until actually executed on its intended host.
Linux compilation does not prove macOS permission, GUI or Keychain behavior.

Outbound receipts have a separate 2 MiB canonical UTF-8 JSON result budget,
including JSON escaping and metadata. A local read may admit up to 8 MiB but
cannot return a larger serialized result through this channel. Exceeding the
receipt budget produces a durable `failed` receipt for read-only file results
with `local_result_too_large`; other successful actions become `unknown`
because their side effects may already have occurred. The oversized body is
not persisted or uploaded. The command reservation remains consumed, and
reconnect/retransmission delivers the bounded receipt without re-execution.
Storage or other outbox validation failures still propagate; they are not
misreported as a result-budget failure.

### Cross-platform vault integration (source implementation)

`credentials::native_secure_store(service)` selects macOS Keychain, Windows
Credential Manager with CurrentUser DPAPI sealing, or Linux Secret Service via
root-owned `/usr/bin/secret-tool` and `/usr/bin/gdbus`. Linux requires an already
running `org.freedesktop.secrets` session service; unavailable/locked vaults fail
closed. No plaintext file fallback is provided. Windows credentials are scoped
to the logged-in user; service namespaces are also authenticated by DPAPI entropy.
Linux subprocesses and pipe reads have a ten-second limit; diagnostics never
include client stderr or secret values. Secret values travel on stdin, not argv.

CLI `secure_store` now calls `credentials::native_secure_store(service)` with
separate identity and outbound namespaces. Unix serve injects the identity signer
into the HTTPS transport. New pairings explicitly negotiate request-proof v1.

`signing::device_proof::DeviceRequestProof::issue` implements a domain-separated
Ed25519 possession proof binding the device, exact HTTPS target and method, body
hash, credential hash, server nonce and a maximum 60-second window. Its explicit
key class is `os-vault-software-ed25519`; these adapters do **not** claim a
non-exportable TPM/Secure Enclave key. The current v2 pairing proof remains intact.
Request-proof registration, nonce issuance/atomic consumption, and client transport
are implemented in source. Activation requires the combined unnumbered SQL
proposal in `docs/design/local-node-owner-emergency-stop.sql.proposal` through the
canonical authority. Existing v2 credentials remain legacy until explicit
re-pairing; new pairings require the successor. See the adjacent design document
for exact transaction boundaries and the unrun real TLS/replay acceptance matrix.
Hardware P-256 providers, tenant hardware-key policy, target-OS vault acceptance
and end-to-end replay acceptance remain unfinished work.
