# Native Assistant SDK

`ai_platform_client::AssistantClient` exposes `chat`, `sessions`, `knowledge`,
`images`, `artifacts`, `tools`, `skills`, `runs`, `threads`, and `mcp` modules.
Construct it with `sdk::AuthConfig`; configure an HTTPS origin or explicit
loopback HTTP address. API-key owner headers remain separate from desktop bearer
sessions. Keys never appear in Debug implementations or error text.

Typed requests and responses live in `sdk_models`; `ChatRequest::new` preserves
chat defaults and optional-field omission. `SessionCreateRequest` stores a title
inside metadata. Streaming accepts an mpsc sender and watch cancellation receiver;
dropping the receiver closes the body, and a terminal event ends observation.

Image async admission is one POST. A server/transport failure or malformed receipt
returns `Error::UnknownOutcome`; callers must inspect existing task/run state and
must not blindly resubmit. Image cancellation remains explicitly unsupported.
Artifact downloads stay on the authenticated Gateway without redirects. Immutable
Knowledge source reads verify document/version/hash/content and a two-MiB cap.

MCP connections are explicit stdio or HTTPS/literal-loopback HTTP configurations.
The client implements initialize/discovery, tool calls and namespaced manager
lifecycle, with bounded JSON/SSE framing and no credential-bearing redirects.

`SDK.md` is a Rust usage surface description, not a claim of live provider or
platform acceptance. The migration report identifies outstanding parity gates.
