# Assistant SDK: native Rust migration

The Python implementation has been retired from this project. Use the native
`ai-platform-client` crate in
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
