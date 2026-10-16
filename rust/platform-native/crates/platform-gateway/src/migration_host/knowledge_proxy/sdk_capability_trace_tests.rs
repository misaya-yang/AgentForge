//! The exact SDK correlation function on real reqwest builders; no DB or SDK
//! provider needed. Loopback verifies transport only, not Runtime authentication.
use super::correlate_runtime_request;
use crate::migration_host::context::{self, OutboundContext};
use axum::http::HeaderValue;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

const TRACE: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
const RUN: &str = "10000000-0000-4000-8000-000000000002";
const OWNER: &str = "10000000-0000-4000-8000-000000000004";
fn context() -> OutboundContext {
    OutboundContext {
        request_id: "sdk-correlation".into(),
        trace_carrier: BTreeMap::from([
            ("traceparent".into(), TRACE.into()),
            ("tracestate".into(), "vendor=value".into()),
            ("authorization".into(), "Bearer forbidden-jwt".into()),
            ("cookie".into(), "forbidden=1".into()),
            ("baggage".into(), "private=forbidden".into()),
            ("x-ai-platform-internal-token".into(), "forged-token".into()),
            ("x-ai-tenant-id".into(), "forged-tenant".into()),
            ("x-ai-user-id".into(), "forged-user".into()),
            ("x-ai-session-id".into(), "forged-session".into()),
            ("x-ai-run-id".into(), "forged-run".into()),
            ("x-ai-turn-id".into(), "fabricated-turn".into()),
            ("x-ai-execution-id".into(), "fabricated-execution".into()),
            ("x-execution-owner-id".into(), "forged-owner".into()),
            ("x-execution-fence".into(), "999".into()),
        ]),
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap()
}
fn body() -> Value {
    json!({"tool_call_id":"official-sdk-call","tool":"tool-name","namespace":"tool-space","arguments":{"query":"exact-fixture","nonce":"unchanged","proof":"unchanged"}})
}
fn request(client: &reqwest::Client, origin: &str) -> reqwest::RequestBuilder {
    let mut token = HeaderValue::from_static("configured-sensitive-fixture-token");
    token.set_sensitive(true);
    client
        .post(format!("{origin}/internal/v1/sdk-runs/{RUN}/capabilities"))
        .header("x-ai-platform-internal-token", token)
        .header("x-ai-tenant-id", "admitted-tenant")
        .header("x-ai-user-id", "admitted-user")
        .header("x-ai-session-id", "admitted-session")
        .header("x-execution-owner-id", OWNER)
        .header("x-execution-fence", "7")
        .json(&body())
        .timeout(Duration::from_secs(45))
}
#[tokio::test]
async fn sdk_correlation_preserves_sensitive_auth_owner_fence_and_exact_request() {
    let client = client();
    let built = context::scoped(context(), async {
        correlate_runtime_request(request(&client, "http://runtime.invalid"), RUN)
            .unwrap()
            .build()
            .unwrap()
    })
    .await;
    let h = built.headers();
    assert_eq!(h["x-request-id"], "sdk-correlation");
    assert_eq!(h["traceparent"], TRACE);
    assert_eq!(h["tracestate"], "vendor=value");
    assert_eq!(h["x-ai-run-id"], RUN);
    assert_eq!(
        h["x-ai-platform-internal-token"],
        "configured-sensitive-fixture-token"
    );
    assert!(h["x-ai-platform-internal-token"].is_sensitive());
    for (name, expected) in [
        ("x-ai-tenant-id", "admitted-tenant"),
        ("x-ai-user-id", "admitted-user"),
        ("x-ai-session-id", "admitted-session"),
        ("x-execution-owner-id", OWNER),
        ("x-execution-fence", "7"),
    ] {
        assert_eq!(h[name], expected);
    }
    for name in [
        "authorization",
        "cookie",
        "baggage",
        "x-ai-turn-id",
        "x-ai-execution-id",
    ] {
        assert!(!h.contains_key(name));
    }
    assert_eq!(built.timeout(), Some(&Duration::from_secs(45)));
    assert_eq!(built.method(), reqwest::Method::POST);
    assert_eq!(
        built.url().path(),
        format!("/internal/v1/sdk-runs/{RUN}/capabilities")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(built.body().unwrap().as_bytes().unwrap()).unwrap(),
        body()
    );
    assert!(context::current().request_id.is_empty());
}
#[tokio::test]
async fn sdk_missing_or_malformed_active_carrier_never_invents_a_managed_turn() {
    let client = client();
    for trace in [
        BTreeMap::new(),
        BTreeMap::from([
            ("traceparent".into(), "invalid".into()),
            ("tracestate".into(), "x".repeat(513)),
        ]),
    ] {
        let captured = OutboundContext {
            request_id: "unsafe request\n".into(),
            trace_carrier: trace,
        };
        let built = context::scoped(captured, async {
            correlate_runtime_request(request(&client, "http://runtime.invalid"), RUN)
                .unwrap()
                .build()
                .unwrap()
        })
        .await;
        let h = built.headers();
        assert!(h["x-request-id"].to_str().unwrap().starts_with("svc-"));
        for name in [
            "traceparent",
            "tracestate",
            "x-ai-turn-id",
            "x-ai-execution-id",
        ] {
            assert!(!h.contains_key(name));
        }
        assert_eq!(h["x-ai-run-id"], RUN);
        assert!(h["x-ai-platform-internal-token"].is_sensitive());
    }
}
#[tokio::test]
async fn sdk_correlation_rejects_malformed_run_before_send() {
    let client = client();
    for run in ["bad run".to_owned(), "bad\nrun".to_owned(), "x".repeat(129)] {
        let result = context::scoped(context(), async {
            correlate_runtime_request(request(&client, "http://runtime.invalid"), &run)
        })
        .await;
        assert!(result.is_err());
    }
}
#[tokio::test]
async fn sdk_wire_uncertain_post_preserves_headers_and_is_not_replayed() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let expected_body = serde_json::to_vec(&body()).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n")
                && bytes.len() >= end + 4 + expected_body.len()
            {
                assert_eq!(&bytes[end + 4..], expected_body.as_slice());
                break;
            }
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() < 8192);
        }
        drop(socket);
        let second = tokio::time::timeout(Duration::from_millis(500), listener.accept())
            .await
            .is_ok();
        tx.send((bytes, second)).unwrap();
    });
    let client = client();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        context::scoped(context(), async {
            correlate_runtime_request(request(&client, &origin), RUN)
                .unwrap()
                .send()
                .await
        }),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    let (bytes, second) = rx.await.unwrap();
    assert!(!second, "uncertain SDK POST was replayed");
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with(&format!("POST /internal/v1/sdk-runs/{RUN}/capabilities ")));
    for (name, value) in [
        ("x-request-id", "sdk-correlation"),
        ("traceparent", TRACE),
        ("x-ai-run-id", RUN),
        ("x-execution-owner-id", OWNER),
        ("x-execution-fence", "7"),
    ] {
        assert!(text.contains(&format!("{name}: {value}\r\n")));
    }
    for name in [
        "authorization:",
        "cookie:",
        "baggage:",
        "x-ai-turn-id:",
        "x-ai-execution-id:",
    ] {
        assert!(!text.contains(name));
    }
    server.await.unwrap();
}
