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

The current package is deliberately headless and dependency-free. It provides:

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
- a dependency-free `local-node doctor` command that never starts networking,
  reads credentials, or exposes a server/listener.

It does **not** expose an HTTP listener, install a daemon, inherit shell
credentials, infer macOS Accessibility/Screen Recording permission, or select a
native backend from the standalone CLI. A future packaged composition may wrap
`LocalNodeRuntime`, but must preserve the boundary validators and action ledger.

The concrete stdlib transport supports HTTPS with system CA/hostname
verification and refuses redirects. `wss://` is accepted by endpoint policy but
remains unavailable until a WebSocket adapter is explicitly injected; there is
no plaintext downgrade. Pairing requires an injected asymmetric device-proof
signer and stores the opaque server credential only through an injected secure
credential store. This source package intentionally ships neither a native
Keychain adapter for those pairing/transport credentials nor a production
asymmetric pairing signer, so standalone doctor output is `unavailable`. The
separate approval-only Keychain signer does not satisfy those authorities. The
insecure file store and HMAC signers used in tests cannot be selected implicitly
in production.

Run the non-starting diagnostic (exit status `2` means unavailable):

```bash
local-node doctor --endpoint https://control.example.test/local-node
```

There is currently **no native macOS Keychain (or equivalent) adapter for
`DeviceIdentity` or the opaque outbound transport credential in this package**.
The separate Keychain-backed trusted-local approval signer cannot be reused as
pairing or transport authority. `DeviceIdentity.load_or_create(...)` therefore reports secure
credential storage unavailable and pairing remains disabled unless a trusted
packaged companion explicitly injects a real `SecureCredentialStore`. The only
file-backed implementation is named `TestOnlyInsecureFileCredentialStore`; it
must be explicitly selected by tests, reports `test_only_insecure`, and is never
a production fallback. Existing legacy JSON containing a plaintext `key` is
rejected and must be re-paired through secure storage.

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

The dependency-free host runner cannot prove network isolation. Canonical
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

Run focused tests from this directory:

```bash
python -m pytest -q
```

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
