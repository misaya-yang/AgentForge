//! Source tests/proxy/test_streaming.py transport failure regressions.
//! Real loopback reqwest transport; only availability discovery is pre-cached.
use super::*;
use tokio::net::TcpListener;
fn request(url: String) -> ProxyRequest {
    ProxyRequest {
        config: ProxyServiceConfig {
            service_id: "source-stream".into(),
            upstream_url: url,
            streaming_concurrency_limit: Some(1),
            timeout_read: 0.1,
            ..Default::default()
        },
        user: CompatUser {
            tenant_id: "tenant".into(),
            request_id: "source-stream".into(),
            ..Default::default()
        },
        api_key_id: None,
        method: Method::POST,
        path: "/runs/stream".into(),
        body: Bytes::from_static(b"{}"),
        headers: HeaderMap::new(),
        query: vec![],
        stream: true,
        langgraph_body_prepared: true,
        effective_model: None,
        effective_provider: None,
        capacity_budgets: vec![],
    }
}
async fn fixture(url: &str) -> Arc<TransparentProxy> {
    let proxy = Arc::new(TransparentProxy::default());
    proxy.availability.lock().await.insert(
        "source-stream".into(),
        Availability {
            checked: Instant::now(),
            checked_at: 0.0,
            urls: vec![url.into()],
            error: String::new(),
        },
    );
    proxy
}
fn assert_released_once(proxy: &TransparentProxy) {
    let slots = proxy.slots.lock().unwrap();
    let slot = slots
        .get(&("source-stream".into(), true))
        .expect("production acquired streaming slot");
    assert_eq!(
        slot.available_permits(),
        1,
        "one-slot permit restored exactly once"
    );
    assert!(
        proxy
            .balance
            .lock()
            .unwrap()
            .connections
            .values()
            .all(|count| *count == 0),
        "upstream connection ownership released"
    );
}
#[tokio::test]
async fn test_streaming_timeout_handling() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = fixture(&url).await;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let _socket = socket;
        std::future::pending::<()>().await;
    });
    let result = tokio::time::timeout(Duration::from_secs(3), proxy.proxy(request(url)))
        .await
        .expect("bounded header timeout");
    server.abort();
    let error = result.err().expect("upstream timed out before SSE headers");
    assert_eq!(error.status, 504);
    assert_eq!(error.detail, "Upstream unavailable");
    assert_released_once(&proxy);
}
#[tokio::test]
async fn test_connection_error_handling() {
    // A non-HTTP loopback peer accepts then closes, yielding an actual request error.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = fixture(&url).await;
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        drop(socket);
    });
    let result = tokio::time::timeout(Duration::from_secs(3), proxy.proxy(request(url)))
        .await
        .expect("bounded connection failure");
    server.await.unwrap();
    let error = result.err().expect("peer closed before SSE headers");
    assert_eq!(error.status, 502);
    assert_eq!(error.detail, "Upstream unavailable");
    assert_released_once(&proxy);
}
