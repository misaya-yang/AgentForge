use super::*;
use crate::migration_host::context::{OutboundContext, scoped_with_budget};
use ai_platform_capability_contract::private_transport::{BUDGET_HEADER, Budget, DEADLINE_HEADER};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const PARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
async fn fixture(
    delay: Duration,
) -> (
    String,
    tokio::sync::oneshot::Receiver<String>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        seen.fetch_add(1, Ordering::SeqCst);
        let mut data = vec![];
        let mut chunk = [0; 1024];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            if n == 0 {
                return;
            }
            data.extend_from_slice(&chunk[..n]);
            if data.windows(4).any(|v| v == b"\r\n\r\n") {
                break;
            }
            assert!(data.len() < 16384);
        }
        let _ = sender.send(String::from_utf8(data).unwrap());
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n").await.unwrap();
        tokio::time::sleep(delay).await;
        let _ = stream.write_all(b"{\"ok\":true}").await;
    });
    (
        format!("http://{address}/internal/v1/threads"),
        receiver,
        calls,
        task,
    )
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
fn trace() -> OutboundContext {
    OutboundContext {
        request_id: "stage-d-request".into(),
        trace_carrier: BTreeMap::from([
            ("traceparent".into(), PARENT.into()),
            ("tracestate".into(), "vendor=value".into()),
            ("baggage".into(), "secret=not-forwarded".into()),
        ]),
    }
}
#[tokio::test]
async fn actual_one_shot_command_preserves_auth_trace_and_private_budget() {
    let (url, received, calls, task) = fixture(Duration::ZERO).await;
    let now = Instant::now();
    let wall = context::wall_ms();
    let parent = Budget::admit(Duration::from_secs(2), wall + 2000, wall, now).unwrap();
    let response = scoped_with_budget(trace(), Some(parent), async {
        let correlation = context::internal_http_headers(
            BTreeMap::from([
                ("x-ai-tenant-id".into(), "tenant-test".into()),
                ("x-ai-platform-internal-token".into(), "fixture-auth".into()),
            ]),
            None,
            None,
            None,
        )
        .unwrap();
        let mut request = client()
            .post(url)
            .json(&serde_json::json!({"admitted":"unchanged"}));
        for (k, v) in correlation {
            request = request.header(k, v);
        }
        // Duplicate/untrusted-looking hints in a builder cannot override the installed ceiling.
        request = request
            .header(BUDGET_HEADER, "999999999")
            .header("X-AI-Budget-MS", "999999999");
        send_json(request, Duration::from_secs(300), "TEST_COMMAND_FAILED")
            .await
            .unwrap()
    })
    .await;
    assert!(response.status().is_success());
    assert_eq!(response.json().await.unwrap()["ok"], true);
    let raw = received.await.unwrap().to_ascii_lowercase();
    assert!(raw.contains(PARENT));
    assert!(raw.contains("x-ai-tenant-id: tenant-test"));
    assert!(raw.contains("x-ai-platform-internal-token: fixture-auth"));
    assert!(!raw.contains("baggage"));
    assert!(!raw.contains("999999999"));
    let budgets = raw
        .lines()
        .filter_map(|line| line.strip_prefix("x-ai-budget-ms: "))
        .collect::<Vec<_>>();
    assert_eq!(budgets.len(), 1);
    assert!(budgets[0].trim().parse::<u64>().unwrap() <= 2000);
    assert!(raw.contains(DEADLINE_HEADER));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}
#[tokio::test]
async fn headers_do_not_complete_command_deadline_body_is_bounded_without_post_retry() {
    let (url, received, calls, task) = fixture(Duration::from_millis(800)).await;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        send_json(
            client()
                .post(url)
                .json(&serde_json::json!({}))
                .timeout(Duration::from_millis(150)),
            Duration::from_secs(300),
            "TEST_COMMAND_FAILED",
        ),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    let _ = received.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}
#[tokio::test]
async fn expired_installed_budget_prevents_command_dispatch() {
    let now = Instant::now();
    let wall = context::wall_ms();
    let budget = Budget::admit(Duration::from_millis(1), wall + 1, wall, now).unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let result = scoped_with_budget(
        trace(),
        Some(budget),
        send_json(
            client().post("http://127.0.0.1:1/"),
            Duration::from_secs(300),
            "TEST_COMMAND_FAILED",
        ),
    )
    .await;
    assert!(result.is_err());
}
