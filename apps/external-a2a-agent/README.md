# External A2A reference agent (U-SW-03)

This is an independent **customer-infrastructure example**. OpenAI's actual JavaScript
`Runner.run()` owns model/tool iteration. Rust remains the platform service and
operational-tool implementation; neither Worker nor the managed Runtime gains a second
framework loop. No legacy SDK Agent Worker importer is used.

Current acceptance: **not_run**. U-GW-15 is not `direct_verified`. The stock adapter refuses
model/task admission without a short-lived operator-signed activation bound to the exact
configuration, credential hashes, source/dependency lock and signed card. There is no production activation signing command or enabled credential in this example.
An explicit private-origin rehearsal signer bootstraps the isolated acceptance flow below. Offline unit fixtures
never establish product acceptance.

## Verified upstream interface and pins

Metadata and source were read on 2026-10-17; packages, images and models were not installed
or downloaded. Exact direct dependencies:

| Package | Version | Official API/source |
| --- | --- | --- |
| `@openai/agents-core` | `0.20.0` | [Runner, Agent, tool exports](https://github.com/openai/openai-agents-js/blob/v0.20.0/packages/agents-core/src/index.ts), [run options](https://github.com/openai/openai-agents-js/blob/v0.20.0/packages/agents-core/src/run.ts) |
| `@openai/agents-openai` | `0.20.0` | [OpenAIProvider with explicit client](https://github.com/openai/openai-agents-js/blob/v0.20.0/packages/agents-openai/src/openaiProvider.ts) |
| `openai` | `7.38.0` | [Official client](https://github.com/openai/openai-node), [exact registry metadata](https://registry.npmjs.org/openai/7.38.0) |
| `@modelcontextprotocol/client` | `2.3.1` | [Official SDK](https://github.com/modelcontextprotocol/typescript-sdk), [metadata](https://registry.npmjs.org/@modelcontextprotocol/client/2.3.1) |
| `zod` | `4.6.7` | [Exact metadata](https://registry.npmjs.org/zod/4.6.7) |

The core SDK's [official MCPServer interface](https://github.com/openai/openai-agents-js/blob/v0.20.0/packages/agents-core/src/mcp.ts)
defines `connect`, `listTools`, `callToolResult`, cleanup and cache invalidation.
`src/mcp.mjs` implements that transport interface against Gateway's actual stateless
**2026-07-28** profile: `server/discover`, per-request protocol metadata, `mcp-method` and
`mcp-name`. It does not send the older initialization/session handshake. Only allowed
read-only tool descriptors pass; pagination, unsupported versions and SSE fail closed.
Model/tool iteration remains in the official SDK Runner.

The explicit OpenAI provider selects **Chat Completions** (`useResponses:false`) through
Gateway's actual `/v1/chat/completions` governed model API. The platform's existing
`/v1/responses` starts a managed Runtime turn, so this external framework does not use it.
[Official pinned Chat model source](https://github.com/openai/openai-agents-js/blob/v0.20.0/packages/agents-openai/src/openaiChatCompletionsModel.ts)
confirms `maxTokens` maps to `max_tokens`, `responseId` comes from the real completion,
and raw usage comes from the provider. `signal`, `maxTurns`, `tracingDisabled:true` and
an explicit provider client are pinned SDK APIs. When MCP is configured, at least one
actual allowed MCP call must confirm before completion.

A 9 KiB independent package lock was resolved with `--package-lock-only --ignore-scripts
--no-audit`; no `node_modules`, package installation or SDK execution was performed.
Root owns its controlled dependency installation. Activation hashes the lock bytes and
requires its presence. Source, config, model/MCP credentials, peer secret, signing/trust
material and TLS file contents are bound through the source/config/credential fingerprints.


## Trust and execution

Copy `config/agent.example.json` to an absolute **external private** path and replace all
placeholders. Configuration and secret files are mode 0600; public certificates/JWKS may be
0644 but never group/world writable. Keep the state directory 0700, outside the repository.
Use one process per state directory; `process-owner.lock` prevents a second process from
concurrently using the same SQLite journal. After a crash, an operator must prove the old
process is dead before removing **only that lock**. Restart marks active tasks unknown;
it never replays a model/tool call. Do not delete the database/WAL to recover.

The HTTPS listener binds only a private IP. The configured public origin must route to
that TLS listener with the exact Host header. Routes:

- `GET /a2a/.well-known/agent-card.json`: Ed25519 detached JWS, exact `HTTP+JSON` / `1.0`
  interface URL, explicit required task-authority and optional task-by-message extensions.
- `POST /a2a/message:send`: strict v1 `ROLE_USER`, UUID `messageId`, text parts only.
- `GET /a2a/tasks/{taskId}` and `POST /a2a/tasks/{taskId}:cancel` (`{}` body).
- `GET /a2a/tasks:byMessage/{namespacedMessageId}`: owner-scoped, read-only reconciliation;
  a missing task is 404. An uncertain send is never retried as a new operation.

All business routes require bearer, `a2a-version: 1.0` and Gateway's compact Ed25519
`x-agentforge-a2a-task` proof. The verifier pins Gateway JWKS from an operator file, refuses
embedded self-selected keys, and validates issuer, exact audience/path/method, raw body
SHA256, tenant, user, agent, message UUID, <=120-second lifetime and durable one-use JTI.
Unsigned actor headers are rejected. Task lookup/cancel/reconcile must match *all* signed
owner and original message fields. Gateway namespaces the message before dispatch.
The read-only shared native fixture is tested byte-for-byte by Node:
`rust/platform-native/crates/platform-gateway/tests/fixtures/protocols/a2a-task-authority-v1.json`.

Per signed tenant/user, the adapter selects a separate file-held **Gateway** model/MCP
credential. It never reads a provider secret or an environment API key. The only outgoing
origin is the configured Gateway: exact `/v1/chat/completions` and optional `/mcp`. DNS is pinned
to the configured IP, the hostname and explicit CA are verified, and proxy, redirect,
WebSocket and SDK retries are disabled. No HTTP client certificate is supplied. Responses
are capped at 4 MiB; prompts are bounded and not persisted/traced. SQLite stores private
outputs and IDs/hashes/actual usage, not prompts or credentials. Host egress policy must
also restrict this customer process; this example does not provision infrastructure.

Durable message hashes detect conflicting retries. Limits cover concurrent tasks, actor
daily admissions, model dispatches, MCP calls, SDK turns, output tokens and run time.
Cancellation persists intent, aborts the SDK, and confirms canceled only after local
execution settles. An unresolved cancellation returns 504 and can be looked up. Model/MCP
effects remain `unknown` after transport ambiguity, including on a canceled task. Canceled
never means billing refunded or an external effect undone. Context IDs are owner-isolated
correlation groups; this example makes no conversation-history/resume promise.

## Activation and controlled acceptance

The production path below still requires U-GW-15 admission. To break the acceptance
bootstrap cycle, use **only an explicitly isolated rehearsal** first:

- Select an isolated tenant/user and exact model/tool profile, small budgets and explicit
  IPv4 private/loopback or IPv6 ULA/loopback origins with matching TLS certificates. Both
  Gateway and agent origin hostnames must be literal private IPs for rehearsal. Public
  hostnames/origins and public pinned addresses are rejected.
- Produce an operator-signed 600-second rehearsal permit locally, using the signer already
  pinned in `activationKeysFile`. The command cannot issue production permits:

  ```sh
  node apps/external-a2a-agent/src/rehearsal.mjs --config /private/agent.json --signing-key /private/operator-ed25519.pem --kid operator-rehearsal-signer-v1 --tenant isolated-tenant --user isolated-user --output /private/operator-activation.jwt
  ```

  It binds the exact config/source/lock/trust/credential/card/profile fingerprints,
  `admission:rehearsal`, one tenant/user, private address and `u_gw15.status:not_run`.
  Each model/MCP HTTP dispatch rereads/rechecks that permit, so expiry or revocation also
  stops later SDK iterations. Foreign owners/profiles and >900-second rehearsal permits
  are rejected. Task admission mode/profile is durable across restart.
- Run the same controlled E2E/export/verify commands below after runtime/model-call approval.
  Rehearsal verification produces **`rehearsal_verified`** and `rehearsal-verified.json`.
  Neither changing the evidence label nor supplying a later production permit may promote
  that transcript. Production activation still needs the owning U-GW-15 evaluation gate.

1. Admit U-GW-15 schema/config and obtain real `direct_verified` evidence through its owning
   workflow. Configure Gateway's outbound peer for `/a2a/message:send`, pinned IP/CA,
   peer-token file, `agent_card_key_id` and raw32-byte base64url `agent_card_public_key`.
   Its origin/signing key must match the example's pinned Gateway trust file.
2. Provision scoped Gateway model and MCP credentials for each permitted tenant/user.
   Configure an actually permitted read-only MCP tool, small real budgets/price book,
   a separate same-tenant exhausted-model-quota actor (`quotaUserId`/`quotaModel`), a known input-blocked guardrail fixture, another user in the same
   tenant and an actor in another tenant. Do not create identities or change quotas
   implicitly from this tool.
3. With dependencies locked, run read-only preflight:

   ```sh
   node apps/external-a2a-agent/src/inspect.mjs --config /private/agent.json
   ```

   An operator's existing trusted signing workflow may issue a compact Ed25519 JWS with
   `typ=agentforge-external-a2a-activation+jwt`, pinned `kid`, no header JWK. Claims:

   ```json
   {"schema":"agentforge-external-a2a-activation/v1","u_gw15":{"status":"direct_verified","evidence_sha256":"<real sha256>"},"configuration_sha256":"<preflight>","credentials_sha256":"<preflight>","source_sha256":"<preflight>","card_sha256":"<preflight>","gateway_origin":"https://gateway.example","agent_origin":"https://external-agent.example","allow_model_calls":true,"iat":0,"exp":0}
   ```

   Use actual current epoch seconds and at most a one-hour lifetime. Bind the *signed*
   card hash printed by preflight. Changing source, package lock, credentials, config or
   card invalidates activation. No fixture key may be installed as production trust; the known shared fixture public key and `UNIT_TEST_ONLY` pins are explicitly refused by production config.
4. Only after that gate and explicit runtime/model-call approval, start the example in
   customer infrastructure:

   ```sh
   node apps/external-a2a-agent/src/server.mjs --config /private/agent.json
   ```

5. Root wires `platform-tools external-a2a-acceptance` to the supplied native module
   `rust/platform-native/crates/platform-tools/src/external_a2a_acceptance.rs::command`.
   Add `base64 = "0.22"`, `ed25519-dalek.workspace = true` and SQLx with
   `runtime-tokio-rustls/postgres/json` to that crate via the shared dependency owner. With a *new existing private* external evidence directory:

   ```sh
   platform-tools external-a2a-acceptance run /private/acceptance.json --apply --allow-model-calls
   ```

   This performs real Gateway→A2A→SDK→Gateway model/MCP execution, checks actual Chat completion
   IDs/usage and Gateway canonical request IDs and SDK/tool counts, duplicate/conflict behavior, owner get/reconcile, cross-user
   and cross-tenant denial, exhausted quota, guardrail rejection and SDK cancellation.
   It persists `intent.json` before any send. Model credentials and MCP audience-bound
   OAuth JWTs must resolve to the same configured main actor; the quota-only token
   resolves to the separate configured quota actor and never enters the external adapter. Any unknown send stops the command. Inspect
   the intent and use Gateway's read-only reconcile route before proceeding; never delete
   evidence and blindly resend. A cancellation/completion race is inconclusive, not a pass.
   Successful observations still explicitly say `governanceAcceptance: not_run`.
6. The native **actual producer**, not a user-authored evidence JSON, reads committed
   ledger sources and signs them:

   ```sh
   platform-tools external-a2a-acceptance export /private/acceptance.json /private/evidence/observation.json /private/ledger-export.json --read-only
   platform-tools external-a2a-acceptance verify /private/acceptance.json /private/evidence/observation.json /private/evidence/ledger-evidence.jwt
   ```

   `ledger-export.example.json` specifies the admitted operator permit/public pin and
   a separate private PKCS8 auditor signer matching the acceptance config's evidence pin.
   Set `DATA_GATEWAY_EVIDENCE_*` using the [existing role-file/mTLS contract](../../docs/deployment/data-platform/README.md):
   DSN/password/CA/cert/key files, exact Gateway role/physical DB, bounded pool <=2 and
   the actual role-sharing replica count. No environment DSN/password, admin role,
   privilege fallback, new grant or schema mutation is used. Transactions are explicitly
   repeatable-read **READ ONLY**, with tenant-local settings and rollback before release.

   The exact admitted permit claims hash must match the durable task. Retain that
   artifact and complete export within its lifetime. The producer verifies exact tenant/user/peer ownership against the durable A2A effect,
   then joins actual model wire audits, `usage_records`, frozen price-book receipts,
   quota-counter checkpoints, settled dispatch reservations, cache observations,
   guardrail audits and MCP authorization results. It records the actual snapshot and
   per-source digests, copies ledger cost units (with `ledger_cents` precision), preserves
   missing cache counts as `not_reported`, and refuses partial, foreign or unsettled
   successful calls. The separate exhausted-quota actor is verified against its actual
   429 audit and absence of model dispatch/usage. Cancellation sources retain any actual
   unresolved financial reservations as `unknown`; they never assert an external undo.

   Real writers are connected: Gateway public Chat completion/denial/unknown exits,
   authenticated MCP→REST results, and A2A receipt/cancel/budget-denial transactions write
   bounded ID/status/hash receipts into existing `audit_logs`. Prompt bodies, model keys,
   arbitrary errors and external claims about usage/pricing are not stored by these hooks.
   No frozen schema change or new ledger table is required. Missing admitted price-book
   or reservation schema/permissions fail closed without grants or fallback.

Evidence claims use schema `agentforge-external-a2a-ledger-evidence/v1`, exact
`observationSha256` (raw file bytes), `gatewayOrigin`, `tenantId`, `userId`, `peer`, `messageId`,
`iat/exp` <=1h and `uGw15:{status:direct_verified,evidenceSha256}`. Every actual model call
needs exactly one `modelReceipts` row matching `requestId/gatewayRequestId/responseId`, scoped owner,
`receiptId`, actual `inputTokens/outputTokens/cachedInputTokens`, `state:settled`, integer
`amountMicros`, `priceBookVersion`, `budgetReservationId`, `guardrailDecisionId` and
`sourceRowsSha256`. Every actual MCP call needs a scoped `capabilityReceipts` row with its
`requestId`, `authorized:true` and source-row digest. `negativeCases.quota/guardrail/cancel`
must bind the original message IDs and verified source-row digests. These are trusted
projections of committed platform rows; SDK usage or local task flags alone are insufficient.
The synthetic `fixtures/native-evidence.test-only.json` exists solely for cross-language
unit validation and must never be submitted as a real export or activation.

## Offline verification

```sh
node --test apps/external-a2a-agent/test/*.test.mjs
```

Tests use Node built-ins, generated ephemeral keys/private scratch state and an injected
unit runner; no listener, model, framework install, database service or paid request starts.
They cover the real native task wire, JCS/duplicate-key ambiguity, pinning/tampering, owner
isolation, durable idempotency/replay/restart, budgets, unknown effects, cancellation, default
closed activation and pre-transport egress/MCP denial. Native tests additionally validate
Node's signed evidence fixture and receipt-contract negative mutations. There are additional rehearsal/private-origin and actual stateless MCP transport negatives. Root must run those
through `scripts/new/native-container.sh` in the serialized Cargo window. Compilation,
fixtures and isolated tests do not establish the governed end-to-end acceptance.
