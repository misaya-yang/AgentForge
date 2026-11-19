# Local Node owner emergency stop

Implementation is source-complete but its two-table authority proposal is
**unactivated**. Current 023/024 have no safe idle-device command/audit storage:
existing local_node_events must refer to an action receipt. This change does not
fake an execution or repurpose a pairing challenge to store a stop request.
`local-node-owner-emergency-stop.sql.proposal` is deliberately unnumbered. Root
must assign a reviewed successor, measure all raw fingerprints and activate it
through the native authority before the new endpoint can operate. No frozen SQL,
current authority, privilege bypass or direct schema creation is included.

The existing owner-authenticated API aliases now expose:

```
POST /api/v1/local-nodes/{device_id}/emergency-stop
Content-Type: application/json

{"confirmation":"stop-all-local-actions","idempotency_key":"<fresh UUID>"}

GET /api/v1/local-nodes/{device_id}/controls/{command_id}
```

The `/api/v1/assistant/local-nodes` alias has the same routes. Existing trusted
LocalNodesIdentityResolver supplies tenant/user; body/header scope injection is
not authority. Missing/incorrect confirmation, malformed request UUID and extra
body fields are rejected. The accepted response is 202 with durable command ID
and pending status, never a claim that the device already stopped. The status
route returns only owned command/intent/request IDs and pending/acknowledged
metadata, never the random nonce seed or a device credential. Both routes refuse
missing storage explicitly; pairing and existing control remain usable on 024.

A stop transaction locks the owner/device row and idempotency namespace, inserts
a permanent intent/audit row, revokes that device's active grants and marks all
its nonterminal actions cancellation-requested. Same-owner/same-key/same-intent
retries return the original command and do not repeat revocation; a reused key
for a different device/intent conflicts. Grant creation takes the same device
lock and refuses while any owner stop is unacknowledged. No global role or
cross-tenant authority is added. Regrant and companion restart are explicit owner
operations; an ACK does not restore grants or resume local action authority.

Pending owner stops suppress ordinary claims and per-action cancel batching.
They use the existing platform Ed25519 signer and exact companion v2
emergency_stop payload, pinned through the signed pairing/challenge authority.
Every delivery ID and nonce binds the durable command and a CSPRNG seed. Payloads
are stable inside a renewal window; renewed expiry gets a distinct delivery ID
and nonce. Prepared deliveries are append-only, so an authentic receipt for an
older delivered window remains valid after reconnect/renewal.

The companion verifies device, expiry, platform signature and durable replay
identity before stop. Reapplying the exact authenticated emergency stop is
idempotent and repairs a crash between nonce reservation and performing stop.
Nonce/payload mutation still conflicts; claims never gain replay permission.
After actual stop it writes a bounded action-less `control.acknowledged` receipt
to the durable outbox. This extension is emitted only for the durable owner's
`emergency.<UUID>.<64hex>` command IDs; older v2 controls retain their previous
receipt contract. The ACK is also produced for an idle device with no action.

Gateway authenticates the device credential and contiguous receipt sequence,
then accepts a control ACK only when its exact prepared delivery joins a command
for that same tenant/user/device. Acknowledgment time/channel are recorded once;
unknown, foreign or payload-bearing ACKs fail closed. Device receipts remain
claims by the authenticated companion, not OS attestation or evidence that a
human approved an unrelated action. Existing local approval requirements remain.

The proposal grants only Gateway SELECT/INSERT on intent rows, column-limited
ACK UPDATE, and SELECT/INSERT on prepared deliveries. No app/migrator admin
fallback, membership, schema CREATE, ledger permission or PUBLIC grant is added.
The owner-only proposal must be verified against the complete successor bundle.

Primary verification commands (serialized normal build settings apply):

```
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway-foundation --test local_node_emergency_stop
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway-foundation emergency_stop_tests
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway local_node_channel
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-local-node --test runner_recovery
```

Only rustfmt/diff/source checks have run for this change; Cargo/DB/desktop/service
execution has not run. Required live acceptance after authority activation:
owner/tenant refusal, same-key retry and foreign-device conflict, concurrent grant
versus stop, idle and active real-TLS delivery, Keychain-backed signature trust,
large-claim priority, local approval interruption, stop ACK/reconnect/renewal and
unknown/foreign ACK rejection. All real TLS/Keychain/full journey acceptance
remains primary-owned; no fixture result is promoted to live acceptance.

## Request proof v1 (same unnumbered successor proposal)

The proposal now also creates immutable per-channel `local_node_request_keys`
and one-row-per-credential `local_node_request_nonces`. Activate all four tables
with the canonical authority in one transaction after measuring the complete
fingerprints. No frozen migration is modified. Gateway alone receives key
SELECT/INSERT and nonce SELECT/INSERT plus column-limited UPDATE; there is no key
UPDATE, DELETE, PUBLIC grant, or new application role.

New native CLI pairing signs `request_proof_version: 1` inside the existing v2
redemption proof. The challenge owner remains the only source of tenant/user
identity. Registration and credential creation commit in the same transaction.
Concurrent pairing is serialized per device; after a device has a PoP registration,
legacy re-pairing cannot downgrade it. The stored credential records that PoP is
required. Old v2 credentials remain explicitly legacy until owner-authorized
re-pairing. New CLI pairing against a pre-successor database fails closed.

For each heartbeat, the native HTTPS transport obtains `kind: request_nonce`
with the bearer credential on the same TLS endpoint, then attaches a base64url
JSON `X-Local-Node-Proof` over the exact outgoing bytes. The signed domain binds
method, complete HTTPS target, body SHA256, credential SHA256, device, registered
public key, key class, nonce and a maximum 60-second window. Gateway derives the
scope from the live credential/device/channel join, verifies the registered key
and exact request, then atomically changes an unused matching nonce to consumed.
One outstanding nonce is retained per credential: repeated challenge requests
cannot replace a live nonce. Replays and concurrent reuse affect zero rows and
are refused. Consumption commits before heartbeat work; a later processing error
requires a fresh nonce and never replays a POST. Existing durable receipt/action
idempotency remains responsible for uncertain transport results.

Native CLI `secure_store` now calls the cross-platform factory. Unix `run_serve`
injects the identity signer into transport. No runner or shared Cargo wiring is
needed. Windows filesystem dispatch remains unavailable in the existing CLI;
Windows vault/identity source support does not claim full Windows execution.

Verification to run centrally: Local Node library/CLI tests and Gateway library
request-proof tests; activate the combined proposal only through authority; then
exercise TLS pairing, signed heartbeat, missing proof, wrong registered key,
foreign credential/device, target/body/nonce tampering, expiry, and simultaneous
replay (exactly one acceptance). Verify old v2 compatibility and re-pair downgrade
refusal. These are source implementations and unrun test cases, not live database
or OS acceptance evidence. Hardware P-256 keys and tenant hardware-key admission
remain separate unfinished U-CL-02 requirements; the only accepted new key class
is explicitly `os-vault-software-ed25519`.
