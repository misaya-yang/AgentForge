//! Real loopback on the production request method; no Worker/DB process needed.
use super::*;
use ai_platform_capability_contract::private_transport::Budget;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
async fn server(
    delay: Duration,
) -> (
    String,
    tokio::sync::oneshot::Receiver<String>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        seen.fetch_add(1, Ordering::SeqCst);
        let mut raw = vec![];
        let mut chunk = [0; 1024];
        loop {
            let n = socket.read(&mut chunk).await.unwrap();
            if n == 0 {
                return;
            }
            raw.extend_from_slice(&chunk[..n]);
            if let Some(header_end) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&raw[..header_end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                if raw.len() >= header_end + 4 + length {
                    break;
                }
            }
            assert!(raw.len() < 16384);
        }
        let _ = tx.send(String::from_utf8(raw).unwrap());
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n").await.unwrap();
        tokio::time::sleep(delay).await;
        let _ = socket.write_all(b"{\"ok\":true}").await;
    });
    (format!("http://{addr}"), rx, count, task)
}
fn scope() -> CapabilityScopeV2 {
    CapabilityScopeV2 {
        tenant_id: "tenant-fixture".into(),
        user_id: "user-fixture".into(),
        session_id: "session-fixture".into(),
    }
}
fn parent(milliseconds: u64) -> Budget {
    let wall = crate::http_service::transport_budget::wall_ms();
    Budget::admit(
        Duration::from_millis(milliseconds),
        wall + milliseconds,
        wall,
        Instant::now(),
    )
    .unwrap()
}
fn client(origin: &str, budget: Budget) -> CapabilityWorkerClient {
    CapabilityWorkerClient::new(
        Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        origin,
        "configured-auth-fixture",
    )
    .unwrap()
    .with_transport_budget(budget)
    .with_execution_owner("owner-fixture".into(), 7)
}
#[tokio::test]
async fn actual_post_propagates_attenuated_budget_without_mutating_signed_body() {
    let (origin, rx, count, task) = server(Duration::ZERO).await;
    let body = json!({"lease":{"signature":"unchanged-signed-proof"},"arguments":{"private":"unchanged"},"idempotency_key":"same-call"});
    let result: Value = client(&origin, parent(2000))
        .request(Method::POST, "executions", &scope(), Some(&body), None)
        .await
        .unwrap();
    assert_eq!(result["ok"], true);
    let raw = rx.await.unwrap();
    let (headers, received_body) = raw.split_once("\r\n\r\n").unwrap();
    assert_eq!(serde_json::from_str::<Value>(received_body).unwrap(), body);
    let headers = headers.to_ascii_lowercase();
    assert!(headers.contains("x-ai-platform-internal-token: configured-auth-fixture"));
    assert!(headers.contains("x-runtime-execution-fence: 7"));
    assert!(headers.contains("x-ai-tenant-id: tenant-fixture"));
    assert!(!headers.contains("baggage"));
    let budgets = headers
        .lines()
        .filter_map(|l| l.strip_prefix("x-ai-budget-ms: "))
        .collect::<Vec<_>>();
    assert_eq!(budgets.len(), 1);
    assert!(budgets[0].parse::<u64>().unwrap() <= 2000);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}
#[tokio::test]
async fn body_stall_expires_same_budget_and_never_reposts() {
    let (origin, rx, count, task) = server(Duration::from_millis(800)).await;
    let result: Result<Value, _> = tokio::time::timeout(
        Duration::from_secs(2),
        client(&origin, parent(150)).request(
            Method::POST,
            "executions",
            &scope(),
            Some(&json!({"same_call":true})),
            None,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        Err(CapabilityWorkerError::Timeout | CapabilityWorkerError::InvalidResponse)
    ));
    let _ = rx.await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}
#[tokio::test]
async fn expired_parent_prevents_any_dispatch() {
    let worker = client("http://127.0.0.1:1", parent(1));
    tokio::time::sleep(Duration::from_millis(5)).await;
    let result: Result<Value, _> = worker
        .request(Method::POST, "executions", &scope(), Some(&json!({})), None)
        .await;
    assert_eq!(result.unwrap_err(), CapabilityWorkerError::Timeout);
}

#[tokio::test]
async fn expired_write_budget_does_not_block_scoped_status_or_events_or_renew_post() {
    let id = "00000000-0000-0000-0000-000000000001";
    for suffix in ["", "/events"] {
        let (origin, rx, count, task) = server(Duration::ZERO).await;
        let worker = client(&origin, parent(1));
        tokio::time::sleep(Duration::from_millis(5)).await;
        let path = format!("executions/{id}{suffix}");
        let result: Value = worker
            .request::<(), _>(Method::GET, &path, &scope(), None, Some(0))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
        let raw = rx.await.unwrap().to_ascii_lowercase();
        assert!(raw.starts_with(&format!("get /internal/v2/capabilities/{path}")));
        assert!(raw.contains("x-ai-tenant-id: tenant-fixture"));
        assert!(raw.contains("x-runtime-execution-fence: 7"));
        assert!(raw.contains("x-ai-platform-internal-token: configured-auth-fixture"));
        let remaining = raw
            .lines()
            .find_map(|l| l.strip_prefix("x-ai-budget-ms: "))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(remaining > 0 && remaining <= 30000);
        let create: Result<Value, _> = worker
            .request(
                Method::POST,
                "executions",
                &scope(),
                Some(&json!({"same_call":true})),
                None,
            )
            .await;
        assert_eq!(create.unwrap_err(), CapabilityWorkerError::Timeout);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.await.unwrap();
    }
}
#[test]
fn readback_exception_cannot_match_create_cancel_catalog_or_arbitrary_paths() {
    let id = "00000000-0000-0000-0000-000000000001";
    for path in [
        "executions".to_owned(),
        "catalog".to_owned(),
        format!("executions/{id}:cancel"),
        format!("executions/{id}/events/extra"),
        format!("executions/{id}?replace=1"),
    ] {
        assert!(!existing_outcome_read(&Method::GET, &path));
    }
    for method in [Method::POST, Method::PUT, Method::DELETE] {
        assert!(!existing_outcome_read(&method, &format!("executions/{id}")));
    }
}
