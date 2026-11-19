# Native SDK Agent Worker

The private `sdk-agent-worker` service and its
`ai-platform-sdk-agent-engine` child are Rust executables. The retained
`openai_agents` and `claude_agent_sdk` framework identifiers preserve the
public service contract; execution uses native Responses/Messages protocol
adapters, rather than the official Python SDKs. See
[the source checkpoint](../handoff/RUST_SOURCE_CHECKPOINT_20261015.md) for
compatibility and acceptance limits.

## Configuration

End users configure a write-only provider key, an entitled model and instructions.
Worker URLs/tokens, provider origins, permissions and filesystem paths are
operator inputs. Gateway detail responses must not expose provider keys.

Operators provide:

- `SDK_AGENT_WORKER_TOKEN`: 32–4096 printable non-whitespace ASCII characters.
  Direct deployments may instead use `SDK_AGENT_WORKER_TOKEN_FILE`; setting both
  fails closed.
- `SDK_AGENT_STATE_ROOT`: an absolute private directory owned by the worker,
  mode0700, with one writer. The image uses `/var/lib/sdk-agent` as UID10001.
- `SDK_AGENT_RETENTION_SECONDS`: 3600–7776000.
- `SDK_AGENT_GATEWAY_URL`: the verified private Gateway origin used by run-bound
  authority and tool callbacks. The sidecar default is
  `http://127.0.0.1:8080`.
- Optional `SDK_AGENT_ENGINE_PATH`: an absolute native engine path. By default
  the engine is beside the worker executable.
- Optional `OPENAI_BASE_URL` and `ANTHROPIC_BASE_URL`: operator-owned provider
  prefixes. Empty values retain vendor defaults. Remote endpoints require HTTPS;
  HTTP is allowed only for an explicitly configured loopback listener.
  Credentials, query strings, fragments, control characters and backslashes are
  rejected. Provider calls do not follow redirects or use implicit retries.
  For example, an OpenAI-compatible prefix ending in `/compatible-mode/v1`
  selects `/compatible-mode/v1/responses`; an Anthropic prefix ending in
  `/apps/anthropic` selects `/apps/anthropic/v1/messages`.

The child receives a scrubbed environment and only validated operator overrides.
Request bodies cannot select provider destinations. Keep all tokens and provider
credentials in private runtime inputs; never place them in build arguments or logs.

## Deployment and source builds

Read [runtime and secret rules](../harness/runtime-and-secrets.md) before
operating any local deployment. Check Compose working-directory and project labels
before replacing a service.

`deploy/compose/docker-compose.sdk-agent-worker.yml` is the explicit `sdk-agents` sidecar
overlay. It shares the native Gateway network namespace, listens only on
`127.0.0.1:8096`, and publishes no port. Gateway uses the same loopback URL.
Recreate the sidecar when replacing its Gateway network namespace. It must not
be exposed through frontend/Nginx routing. SDK readiness is separate from core
Assistant readiness.

The source image target is `sdk-worker` in
`deploy/platform-native/Dockerfile`. The repository-root build context supplies
the locked native workspace and independent native engine. The image contains
both binaries and runs as UID/GID10001, with private durable state, a read-only
root, no capabilities, bounded memory/CPU/processes, private temporary storage
and `init: true`. The old apps/SDK Python locks and wheel commands are archived
references and are not build inputs.

The namespace is not a provider-only egress policy or a hard tenant sandbox.
Separate-node deployments need operator-managed private TLS and appropriate
network policy. Do not weaken callback validation to accommodate a bridge-host
plaintext URL.

## Persistence and recovery

The worker owns scoped request receipts, history, pending execution and cleanup
markers. Unknown execution stays fenced after disconnect or restart; reconnect
must not repeat a provider or governed tool operation. A completed receipt is
replayable only under the original request fingerprint and current authority.
Deletion and retention retain permanent scope tombstones and must not clear an
unresolved execution merely to restore availability.

The native engine persists bounded native history. It can read the scoped
Responses input records from an existing OpenAI SQLite session without modifying
that database. An opaque Claude SDK session identifier is not Messages history:
such a continuation fails closed until explicitly migrated. Do not substitute an
empty history and claim continuation.

Process cleanup observes known handles and signal outcomes. A leader exit does
not establish that every descendant or accepted remote provider operation stopped.
Reconcile uncertain state only after an authorized operator establishes containment
and preserves the original scoped pending, receipt and tombstone evidence. There
is no automatic marker-clearing recovery endpoint.

## Verification

Use the pinned toolchain, one Cargo job, no debug/incremental output and an
external `CARGO_TARGET_DIR`. These commands are native source/contract gates:

```sh
cargo test --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-sdk-agent-worker --bin sdk-agent-worker
cargo test --locked \
  --manifest-path rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/Cargo.toml
```

`platform-sdk-agent-worker/tests/native_stack.rs` supplies the opt-in composed
worker/child/provider/authority fixture. Follow its explicit feature and binary
prerequisites; a skipped or ignored test is not acceptance. Fixture provider
responses establish local protocol and lifecycle behavior, not real vendor/model
support or official SDK compatibility.

The current-source composed fixture passed on macOS with the actual worker and
engine binaries. It covers Responses provider-prefix routing, history, refusal
output, approval/rejection callbacks and cancellation reconciliation. The
cancellation repair reaps an exited process-group leader before treating a
zombie-only group's `EPERM` as failure; cleanup succeeds only after the original
group is confirmed absent. Inaccessible or surviving groups remain fenced.
Gateway-mediated real-provider and restart acceptance are still pending.

`sdk-agent-worker health` authenticates against the private readiness endpoint
without a paid provider call. Actual current-source acceptance must separately
prove configured provider execution, persisted multi-turn history, receipt replay,
allow/deny tool decisions, cancellation and restart refusal through Gateway and
the worker. Do not turn a compile, health200 or archived test count into release
acceptance.
