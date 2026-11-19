# Native SDK-protocol HTTP service migration

## Delivered source

- `rust/platform-native/crates/platform-sdk-agent-worker` builds the private
  `sdk-agent-worker` service (entrypoints `serve` and `health`).
- Uses the separate native `ai-platform-sdk-agent-engine` executable supplied by
  the runtime migration lane. No Python execution/delegation exists in this service.
- The complete Python worker package was read as the behavior specification:
  contract, launch frame, server, lifecycle, supervision, runtime, knowledge,
  capabilities, engines, tools, usage, child and official-results tests.

## Preserved HTTP and durable lifecycle contracts

- Authenticated `POST /v1/runs`, `DELETE /v1/sessions`, `GET /health/ready`;
  protocol `ai-platform/sdk-agent/v1`; strict request shape and typed bounds;
  provider credentials accepted only from the private request header and passed
  to the engine in a bounded stdin JSON+LF envelope, never argv or logs.
- SHA-256 private scope hashing preserves Python ASCII Unicode/surrogate escapes.
  Canonically sorted fingerprints exclude rotating Knowledge/capability grants.
  Matching completed receipts replay; conflicting reuse is 409; pending runs
  require reconciliation and are never dispatched again.
- Admission and session filesystem locks; 0700 state-root ownership checks;
  no-follow lock/JSON files; atomic replacement, file fsync and directory fsync;
  durable cleanup fences before spawning; cleanup-complete proof after cleanup;
  pending markers retained after failures; permanent deletion/expiry tombstones.
- Strong task ownership survives request disconnect. Uncertain spawn, process
  cleanup, panic or commit failures retain the owner/lock and durable fence and
  make readiness fail. Restarted orphan cleanup markers are never auto-cleared.
- Clean child environment; isolated Unix process group; TERM then bounded KILL
  even if the original leader already exited; child exit validation before
  commit. Cleanup completes before successful receipt publication.
- Output limits: 1 MiB per NDJSON event, 4 MiB aggregate; 16 queued events;
  partial/incomplete/malformed terminal output fails; bounded SSE streams emit
  unchanged delta/completed/approval_required/approval_result/tool_result events.
- Gateway origin policy, no proxy/redirect/retry, bounded authority response;
  selected dataset/profile immutability; pre-launch Knowledge/capability admission;
  ten-second active capability checks and post-cleanup pre-commit revalidation.
- Worker-owned expiry sweep in bounded batches of 100, freshness-gated readiness,
  retention configuration bounds, session busy/deleted/reconciliation responses.
- Graceful signal handling stops admission, cancels active execution and waits for
  bounded cleanup; HTTP drain is bounded even if a client sends an unfinished body.

Skills, image and other platform tools retain their exact governed descriptors,
arguments, approval and receipt transport. The HTTP service introduces no local
model tools or direct image/skills execution; these remain Gateway/Runtime-owned.

## Native execution and compatibility boundaries

The new service is a native provider-protocol implementation, not an embedding of
the official Python OpenAI Agents SDK or Claude Agent SDK. Readiness reports
`implementation: native-provider-protocol`, native package version and an empty
`versions` map instead of fabricating official SDK versions. The latest explicit
all-Rust migration requirement makes literal preservation of those Python SDK
implementations impossible. See the engine's `COMPATIBILITY.md` for model-loop,
Anthropic token ceiling, usage/cost, arithmetic and provider SSE boundaries.

Existing OpenAI SDK history is preserved by a read-only, scope-bound SQLite import
added to the engine under an explicit ownership expansion. The pinned 0.23.1 SDK
stores Responses input objects in `agent_messages.message_data`, ordered by `id`
and filtered by `session_id`. Verified source:
https://raw.githubusercontent.com/openai/openai-agents-python/v0.23.1/src/agents/memory/sqlite_session.py

The import preserves the source database, rejects malformed data, reads at most
10,000 objects / 8 MiB, and publishes native conversation history only after a
successful continued run. An opaque Claude SDK resume ID cannot reconstruct its
conversation through the Messages API; such sessions fail closed and need a
separately verified export/import. They are never silently reset.

## Configuration and integration

Retains `SDK_AGENT_WORKER_TOKEN` or `SDK_AGENT_WORKER_TOKEN_FILE` (mutually
exclusive), `SDK_AGENT_STATE_ROOT`, `SDK_AGENT_RETENTION_SECONDS` and optional
`SDK_AGENT_GATEWAY_URL`. `SDK_AGENT_ENGINE_PATH` may specify the absolute
operator-owned native engine path; default is `ai-platform-sdk-agent-engine`
alongside the service executable. Bind remains private `127.0.0.1:8096`.

The parent coordinator owns workspace membership, lockfiles, builds, deployment
selection and integration acceptance. This lane does not claim a deployed change.

## Verification status

Added offline Rust tests for strict launch contracts, Unicode scope encoding,
grant-independent fingerprints, origin constraints, filesystem session exclusion,
tombstones, restart cleanup fences, cleanup proof and deletion, JSON durability,
HTTP authorization/body rejection, successful receipt replay/conflict, malformed
child terminal fencing, SSE disconnect process-group cleanup and orphan readiness.
Engine tests additionally cover exact scoped/ordered/read-only legacy SQLite import
and malformed legacy row rejection.

No Cargo command was run in this lane (coordinator's serialized build policy).
Initial `rustfmt` command could not run because it is absent from this executor's
PATH. The coordinator's first compile found an `io::Error` equality expression;
it was corrected to a `matches!` guard. The coordinator reported the service check passed at 10:19 UTC. Verified the
coordinator's actual logs: `worker-local-tests.log` reports **13 service tests
passed, 0 failed**; `engine-tests.log` reports **12 engine tests passed, 0 failed**,
including both new legacy SQLite import tests. Logs are under the coordinator's
`full-rust-20261015` verification output. The subsequently removed dead helper is
nonfunctional; any further source changes require the final coordinated rebuild.

Not verified here: paid/live provider calls, actual official SDK equivalence,
end-to-end image/skill approvals, production filesystem crash/power-loss tests,
container shutdown acceptance, or opaque Claude session export/import.

## Composed native integration evidence

The coordinator's `native-stack-test.log` records **1 passed, 0 failed** in
5.20 seconds for `actual_service_engine_provider_approval_cancel_and_history`.
It launches both actual Rust binaries against loopback provider and authority
fixtures and proves streaming/nonstream completion, receipt replay without a
provider call, persisted history reload, approval allow/deny event/result paths,
real TCP SSE disconnect cleanup, failure fencing/deletion and graceful SIGTERM.

Only a non-default `integration-fixtures` feature enables loopback provider
fixture origin injection. Production defaults still pin official provider URLs.
These tests are offline protocol integration, not paid/provider live acceptance.
