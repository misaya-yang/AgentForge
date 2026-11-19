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

## Device login and platformctl

`device_auth::DeviceLogin` uses the generated device-flow request models and the
same no-redirect/no-retry native HTTP client. The public client ID is
`agentforge-cli`, scope `platform`, and S256 PKCE binds approval to the CLI.
Approve the printed verification URI/code in an existing browser session. No
password, API key, tenant selector or refresh token is accepted in argv.

```text
platformctl auth login --origin https://gateway.example
platformctl auth status --origin https://gateway.example
platformctl --origin https://gateway.example tasks list
platformctl auth logout --origin https://gateway.example
```

A strict `--config FILE` accepts only `version: 1`, `origin` and optional
`client_id: "agentforge-cli"`; credentials remain in the OS vault. Normal commands
retrieve/refresh that scoped session. Existing `--token-stdin` is retained for
explicit token injection. Results are JSON; login and watch progress are NDJSON.
The internal `--credential-stdio` protocol requires piped stdin/stdout and is used
only by the signed independent CLI helper. It never prints secrets on a terminal.

Cross-process OS locks serialize refresh/revoke. Before a rotating refresh, the
vault records an in-flight marker. Unknown outcomes or crashes cannot trigger a
blind refresh retry; login must be repeated explicitly. Tenant/user and scope are
validated on every token replacement. Logout removes local credentials even if
remote revocation is uncertain; that uncertainty is not reported as confirmed
revocation. Hardware stores, browser approval and real Gateway/Redis acceptance
remain separate from generated-contract and loopback fixture tests.

Command failures on stderr use
`{"schema_version":"agentforge.platformctl.error.v1","error":{"code":...,"message":...}}`.
Help remains plain text. A request/unknown-outcome error never includes a token or
untrusted server response body. Exit codes remain separate from the JSON result.
