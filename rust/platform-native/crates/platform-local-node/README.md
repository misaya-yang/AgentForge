# Native Local Node

Rust outbound-only companion for the Local Node control-plane contract. It has no
listener, endpoint discovery, ambient filesystem grant, or unsigned pairing path.
Protocol v2 uses the exact HTTPS endpoint
`/api/v1/local-nodes/control`; v1 responses fail closed because v2 verifies a
platform Ed25519 signature bound to the one-use challenge and device identity.

## Headless CLI

The CLI accepts one strict JSON operator config. It contains no credential or
private key. A read-only configuration can omit `approval_helper`; write/rollback
grants require the explicit native approval helper path.

```json
{
  "endpoint": "https://gateway.example.test/api/v1/local-nodes/control",
  "state_directory": "/Users/operator/Library/Application Support/AI-Platform/LocalNode",
  "tls_ca_file": "/Users/operator/.config/ai-platform/platform-ca.pem",
  "display_name": "Research workstation",
  "platform": "macos",
  "node_version": "0.1.0",
  "heartbeat_interval_seconds": 5,
  "trusted_platform_keys": [
    { "key_id": "platform-2026", "public_key": "URL_SAFE_UNPADDED_ED25519_PUBLIC_KEY" }
  ],
  "workspace_grants": [
    {
      "grant_id": "project-notes",
      "root": "/Users/operator/Projects/notes-fixture",
      "tenant_id": "tenant-from-owner-session",
      "user_id": "owner-from-session",
      "capabilities": ["read", "write"]
    }
  ],
  "approval_helper": "/Applications/AI Platform/Local Approval Helper"
}
```

Validate without launching the helper or making a network request:

```text
local-node config validate --file /path/to/local-node.json
```

Create a one-use challenge first through the owner-facing API, then pipe its
user code to pairing so it is not placed in shell history or argv:

```text
printf '%s\n' "$PAIRING_CODE" | local-node pair --config /path/to/local-node.json --challenge-id CHALLENGE_UUID --user-code-stdin
local-node serve --config /path/to/local-node.json
```

Pairing private identity and device credentials are stored through the native
secure credential adapter. The state directory holds only the durable local
ledger, replay/approval nonce stores, outbox and rollback records. The explicit
TLS CA, when present, is the only trust root for that client; certificate and
hostname verification stay enabled.

The Gateway's approved tool record is not a device-local owner approval. For a
write, the companion displays the signed action scope and current target hash,
obtains a separate Keychain-backed local signature, verifies its action/device/
argument/target/policy bindings, durably consumes its random one-use nonce, and
only then writes beneath the configured grant root. Denial, helper failure,
expiry, changed target or replay cannot write. Device signatures establish key
possession; they do not attest macOS privacy permissions or other host policy.

`local-node doctor [--endpoint URL | --config FILE]` is read-only and never
touches credentials, opens workspace roots or starts transport. Read-only CLI
diagnostics and config validation are available without running the companion.

## Local capability boundary

`Grant` pins filesystem device/inode and resolves paths through no-follow open
directory descriptors. File writes require exact signed arguments, a matching
expected target hash, one-use local approval and a durable action ledger record.
Bounded read results and successful write receipts may remain in the outbox only
until contiguous server acknowledgment; terminal receipts are retained through
reconnect.

`ProcessRunner` uses an explicit executable allowlist, a clean environment and a
pinned granted cwd. Requests for network denial fail closed until an enforcing
sandbox is injected. `ComputerController` requires an exclusive session lease,
current observation and approval for each input primitive. The macOS adapter
calls the native AX/CGEvent helper; permission and sensitive-UI denials remain
hard boundaries.

Tests are native Cargo targets. Root coordinates their serialized build and
execution; Linux results do not establish macOS Keychain, Accessibility, screen
permission or GUI acceptance.

## TLS/control acceptance wiring

Gateway enables the v2 companion channel only when all of these explicit inputs
are supplied to the native Gateway process:

- `AI_PLATFORM_LOCAL_NODE_CHANNEL_ENABLED=true`
- `AI_PLATFORM_LOCAL_NODE_PLATFORM_KEY_ID` and private `AI_PLATFORM_LOCAL_NODE_PLATFORM_SIGNING_SEED_B64`
- absolute `AI_PLATFORM_LOCAL_NODE_TLS_CERT_PATH` and `AI_PLATFORM_LOCAL_NODE_TLS_KEY_PATH`
- `AI_PLATFORM_LOCAL_NODE_TLS_LISTEN`, e.g. `127.0.0.1:18443` for owned-machine acceptance

Use the corresponding public Ed25519 key in the operator config and a CA whose
server certificate SAN matches the configured endpoint. Keep the private seed/key
and credential values outside reports and argv. The companion control path is on
this TLS listener; owner pairing/grant APIs and the independently authorized
Worker describe/dispatch/status/cancel routes remain on the normal Gateway
listener. An HTTPS proxy header never enables the plaintext companion endpoint.

For the primary's serialized verification, run affected Gateway tests filtered by
`local_node_channel` and `local_node_tls`, followed by native Local Node
`runner_recovery` and `local_approval_prompt` targets. The TLS unit tests create
an ephemeral external CA/leaf using the existing `openssl` CLI and exercise real
TLS handshakes, including hostname refusal and a stalled unauthenticated TCP peer.
They do not open Keychain, touch the desktop or establish real Gateway/DB acceptance.

The heartbeat assembler now places signed emergency-stop/cancel commands before
claims, retaining a bounded 100-command / 900 KiB response. Deferred claims remain
in the server ledger. Pending cancel payloads are stable across repeated polls
inside a renewal window; fresh command/nonce identities accompany renewed expiry,
so delivery retries do not conflict with the durable companion replay guard.
TLS handshakes run concurrently with a fixed 64-socket cap and individual deadlines;
a single stalled handshake cannot hold the listener for its full timeout.

`local_live_acceptance` remains its documented local-disk/process acceptance with
explicit test-only signers. Do not relabel its receipt as real TLS/secure-store or
owner-approved write evidence. Full acceptance still requires the real CLI pair
and serve journey with Keychain, one-use challenge, owner and local write approval,
remote cancel, emergency stop, reconnect receipts and readback. Source/unit tests
alone certify none of those live results.

The additive owner stop/audit API and durable companion ACK extension are
implemented in [owner emergency stop](../../../../docs/design/local-node-owner-emergency-stop.md).
Its unnumbered schema proposal requires reviewed authority activation; current
024 refuses the new owner operation rather than constructing a fake execution.
