# Generated Python SDK models and client

`generated_models.py` is generated from the current native OpenAPI 3.2 document.
It supplies Python 3.11+ `TypedDict` models for component schemas, operation
parameters, requests, responses and stream items, plus a typed `Client` that
delegates to a caller-provided `TypedTransport`. It has no HTTP dependency and
never invents authentication, CSRF, base URL, retries or SSE parsing.

Generate with `ai-platform-openapi codegen --out sdk`; verify committed artifacts
with the same command plus `--check`. `--candidate /path/to/openapi.json` selects
an explicit offline source. `sdk/generated-manifest.json` records the source SHA,
operation inventory and each operation's generated Python request/stream method.
The server remains the authority for runtime validation and permissions.

Generated V2 event requests accept the optional `header` mapping containing
`Last-Event-ID`. Omit the query cursor to resume from that header; an explicit
numeric `after_sequence`, including zero, wins. See the
[event observation contract](../typescript/README.md#resuming-event-observation)
for replay limits and the separate Local Node JSON feed.

The earlier handwritten Python networking implementation remains retired. The
native Rust transport below is still the reference for authenticated networking.

Use the `ai-platform-client` crate in
[`rust/platform-native/crates/platform-client`](../../rust/platform-native/crates/platform-client).
The API guide is [`sdk/rust/README.md`](../rust/README.md); transport and recovery
contracts are in [`SDK.md`](../../rust/platform-native/crates/platform-client/SDK.md).
Shared JSON event fixtures remain in [`sdk/fixtures`](../fixtures).

`AssistantClient` exposes chat, sessions, knowledge, images, artifacts, tools,
skills, runs, threads and MCP. Requests use typed Rust models; streaming uses a
Tokio channel and explicit cancellation. Unknown mutation outcomes must be
recovered by querying state rather than blindly retried.

## Translated executable examples

From the repository root, with credentials already supplied through your
approved environment:

```bash
# Chat, streaming, Knowledge datasets/question and tool listing.
# GATEWAY_API_KEY is required. Use HTTPS remotely, or a literal loopback IP locally.
GATEWAY_BASE_URL=http://127.0.0.1:8080 cargo run --locked \
  --manifest-path rust/platform-native/Cargo.toml -p ai-platform-client --example demo

# Immutable publication session, optional attachment and SSE stream.
# Requires AGENT_PUBLICATION_ID and AGENT_RUNTIME_TOKEN.
# Optional: AI_GATEWAY_BASE_URL, AGENT_ATTACHMENT_PATH.
cargo run --locked --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-client --example agent_runtime
```

These examples contact the configured service when invoked. The publication
example retains the explicit `sdk-example-turn-1` idempotency key; use a new key
for a deliberately new turn, not to retry an uncertain admission.

## Offline verification

```bash
cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-client
cargo check --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-client --examples
```

Native compilation does not establish live provider acceptance. Migration and
remaining operational boundaries are recorded in
[`reports/rust-migration-local-retirement-map.md`](../../reports/rust-migration-local-retirement-map.md).
