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

## Streaming remote control (U-CL-03)

`local-node serve` opens a second outbound POST on the same exact HTTPS endpoint
with `kind: control_stream`. Gateway returns bounded signed NDJSON frames carrying
cancel/emergency-stop commands. The HTTP response remains open; no node listener
or additional credentials are introduced. The existing heartbeat keeps delivering
claims, doctor state and contiguous receipt acknowledgments. The stream checks the
same SQL credential/device/tenant authority every 200 ms, prioritizes emergency
stop, and sends a signed idle heartbeat at least once per second. Credentials and
revocation are rechecked throughout the connection.

Each frame binds the device, a fresh client-selected stream UUID, a consecutive
sequence, short expiry and pinned platform key. Commands retain their independent
signatures, scope, approval/fence checks and durable nonce guard. After successful
processing the node checkpoints the frame cursor in its replay database and sends
it on reconnect. The fresh connection continues its sequence after that checkpoint
while binding every new frame to its new stream UUID. The server deliberately redelivers any still-pending durable
command even after that cursor: receipts, command IDs and nonces determine whether
work has already executed. A disconnect cannot acknowledge or lose an offline stop.
One active stream per credential replaces an older connection; body drop releases
its lease. Per-stream pending command deduplication and buffers are bounded.

Three seconds without a complete verified frame (including headers or frames held
by a buffering proxy) drops streaming and immediately starts 250 ms polling, then
retries the stream after 30 seconds. The new Gateway permits this bounded fallback
rate per authenticated credential. Explicit unsupported-kind/status responses from
an older Gateway retain the configured heartbeat cadence for compatibility. Local
shutdown interrupts pending open/read operations and joins the existing dispatcher
through its emergency-stop cleanup. NDJSON responses request `no-store,
no-transform` and `X-Accel-Buffering: no`; timeout fallback remains active even when
a proxy ignores them.

Reproducible offline checks, through the shared serialized container workflow:

```text
scripts/new/native-container.sh test -p ai-platform-local-node --lib
scripts/new/native-container.sh test -p ai-platform-local-node --test runner_recovery -- --nocapture
scripts/new/native-container.sh test -p ai-platform-gateway --lib -- local_node --nocapture
```

The runner measures 20 signed-stop deliveries while dispatch is blocked; the
production router measures 20 stream deliveries over an isolated HTTP loopback
fixture, with a p95 < 1 s assertion. Framing tests cover split/coalesced frames,
connection/device/sequence replay, signature tampering, oversized frames and a
buffering proxy. Gateway tests cover request-proof rejection, reconnect replay,
revocation, duplicate suppression and signed idle heartbeats. HTTP fixture timing
and isolated unit results do not establish real TLS/database/proxy or macOS-device
product acceptance. That gate still requires owner-authorized pairing and serve,
a real buffering proxy, and measured remote-stop completion on the physical node.

## Device keys and OS storage (U-CL-02)

`native_secure_store` selects macOS Keychain, Windows Credential Manager with
CurrentUser DPAPI, or Linux Secret Service. It never falls back to a file or
plaintext credential. Names are validated, secret subprocess traffic uses pipes,
operations are bounded, and delete is idempotent. Linux requires trusted,
root-owned `/usr/bin/secret-tool` and `/usr/bin/gdbus` plus an active Secret Service.
No store, locked/unavailable storage or failed readback stops the operation.
Windows namespace/DPAPI tests are platform-specific; Linux fixture tests do not
establish Windows vault or a real desktop Secret Service acceptance.

The existing `software` device-key policy preserves v2 Ed25519 identity and proof
compatibility. Explicit `"device_key_policy": "hardware_required"` selects a
non-exportable P-256 backend and never silently downgrades:

- macOS: Secure Enclave keys in the data-protection keychain, when-unlocked and
  this-device-only access, checked token identity and refused private export.
- Windows: Microsoft Platform Crypto Provider, hardware implementation flag,
  zero export policy and an actual refused private-key export attempt.
- Linux: a root-owned system OpenSSL TPM2 provider and fixed kernel
  `/dev/tpmrm0` TCTI. Only TPM-wrapped `TSS2 PRIVATE KEY` blobs are persisted;
  environment-selected providers, simulators and software key generation are excluded.

The backend exports only a 65-byte SEC1 public point and signs with ES256/DER
ECDSA. Public key metadata is private, bounded and checked against the reopened
backend; a signature self-test runs before pairing/use. This is local key-provider
evidence, **not** remote hardware attestation or process identity attestation.
The Gateway source now verifies both algorithms against the immutable registered
algorithm/class/key, with canonical base64url, uncompressed P-256 SEC1 points and
strict DER ECDSA signatures. Named hardware classes must match the device OS and
are signed into pairing and every request. ES256 requires request-PoP v1; one-use
nonce consumption, tenant/credential/body/target binding and pairing redemption
remain authoritative. Legacy Ed25519 stays compatible.

The unnumbered `database/proposals/local-node-key-assurance.sql` is required for
ES256 and tenant assurance policy; it has not been applied. Missing new schema
keeps legacy Ed25519 behaviour; partial activation or unsupported ES256 fails
closed. Tenant policy can refuse software and unverified P-256 independently, or
require verified hardware and restrict admitted classes. Pairing, nonce issuance,
request authorization, heartbeat/stream reads and worker channel admission all
recheck that policy. A policy rejection terminates observation; the node's
existing runner shutdown invokes emergency stop and cancels active work.

A client-reported hardware class is always **unverified P-256** unless a separately
enrolled attestation authority has admitted exact owner/device/key evidence.
Gateway has SELECT-only access to policy, verifier trust and attestation records;
there is no client/Gateway enrollment or self-upgrade endpoint. Verification
checks an authority-pinned Ed25519 assertion, owner/device/key/OS scope, evidence
hash, attestation nonce, expiry and revocation. It additionally parses native TPM2
Certify evidence, checks the SHA256 name of the exact P-256 public area,
fixedTPM/fixedParent/sensitiveDataOrigin/sign attributes, challenge and safe clock,
and verifies the P-256 AK signature. The enrolled verifier must validate the AK's
restricted hardware origin and endorsement/platform trust chain before signing;
a self-selected AK or a generic device certificate is insufficient.

The retained TPM evidence JSON has version 1 and canonical base64url fields
`public_area` (TPMT_PUBLIC without outer size), `attestation` (TPMS_ATTEST without
outer size), `signature` (ASN.1 DER) and `attestation_key` (65-byte SEC1). The signed
assertion uses domain `ai-platform.local-node.hardware-attestation.v1`, UUID
attestation_id, verifier_id, exact tenant/user/device/public-key SHA256/class/OS,
evidence_format `tpm2-certify`, SHA256 of retained evidence bytes,
attestation_nonce (32 random bytes in canonical base64url), verified_at and
expires_at (at most 90 days). Real evidence and verifier enrollment are an
independent authority workflow; no synthetic fixture is enrolled by this source.
Revoked/expired existing records fail closed rather than silently downgrade.
macOS Secure Enclave remains unverified because this version has no admitted
key-specific Apple attestation flow.

macOS compilation needs the existing security-framework dependency's `OSX_10_15`
feature and a signed application with approved data-protection keychain
entitlements. No database activation, key generation, physical attestation or OS
hardware acceptance was run for this source update.

Windows storage/key providers do not create a Windows filesystem dispatcher or
computer-use driver. The existing unsupported dispatcher boundary remains closed;
U-CL-04 still requires separate product demand. Desktop shell choice remains with
P-ADR-8; this work does not replace the current egui client or add Tauri.
