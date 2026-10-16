# Rust Assistant SDK

The native client is `ai-platform-client`, in
`rust/platform-native/crates/platform-client`. Its `AssistantClient` facade
provides chat, sessions, knowledge, images, artifacts, tools, skills, runs,
threads, and MCP modules. Shared event fixtures remain in `sdk/fixtures`.

## Chat and sessions

```rust,no_run
use ai_platform_client::{AssistantClient, sdk::AuthConfig};
use ai_platform_client::sdk_models::{ChatRequest, SessionCreateRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut auth = AuthConfig::new(std::env::var("GATEWAY_API_KEY")?);
    auth.tenant_id = std::env::var("GATEWAY_TENANT_ID")?;
    auth.base_url = "http://127.0.0.1:8080".into();
    let mut client = AssistantClient::new(auth)?;
    let session = client.sessions.create(SessionCreateRequest {
        title: Some("Example".into()), ..Default::default()
    }).await?;
    let mut request = ChatRequest::new("Explain the refund policy.");
    request.session_id = Some(session.session_id);
    let response = client.send(request).await?;
    println!("{}", response.content);
    client.close().await?;
    Ok(())
}
```

Use HTTPS for remote gateways; local development permits a literal loopback
HTTP address. API-key headers carry tenant/user identity separately from the
desktop client's bearer session. Never place credentials in URLs.

## Streaming

`client.stream(request, sender, cancellation_receiver)` emits `SdkEvent` values
through a Tokio mpsc channel. Events expose `is_text()`, `is_done()`, `is_error()`,
`is_cancelled()`, and `text()`. The caller controls cancellation with a Tokio
watch channel. Dropping the event receiver closes observation. Terminal events
finish the stream without replaying the request.

## Recovery and side effects

Read-only requests support bounded retry/backoff. Mutations are sent once.
`Error::UnknownOutcome` means admission may already have happened: inspect
run/task state and do not blindly resubmit. Cancel only on explicit application
intent. Image-task cancellation is unsupported by the Gateway.

Artifacts download through the authenticated gateway with redirect rejection.
Immutable Knowledge source reads verify dataset, document, version, hash and
content. MCP stdio/HTTP connections are explicit and managed through `client.mcp`.

See the crate's `SDK.md` for transport limits and `sdk_models` for typed requests.
Native integration tests use local HTTP mocks and frozen JSON fixtures, without
Python imports or live provider dependencies.

### Long-running reasoning streams

Streaming header/idle limits are independent from ordinary request timeouts.
Defaults remain 30 seconds for response headers and 90 seconds between body
chunks. Configure `StreamLimits::new(header_timeout, idle_timeout)` with positive
Durations up to 24 hours each, then use
`AssistantClient::new_with_stream_limits(auth, limits)` or
`Client::new(origin)?.with_stream_limits(limits)`. For example, a 10-minute header
limit and one-hour idle limit can accommodate an explicitly chosen long-reasoning
model. Consumer drop and cancellation remain immediate at any configured limit.
No stream setting enables POST retry. Only the V2 GET event observer resumes its
existing server `after_sequence` cursor and suppresses duplicate sequences.
