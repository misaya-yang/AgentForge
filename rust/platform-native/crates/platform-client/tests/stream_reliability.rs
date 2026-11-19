//! Loopback-only transport tests. No provider call or server-side cancellation is issued.
use ai_platform_client::{Client, Error, sdk_stream::SdkDecoder, sse::Decoder};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, watch},
};
async fn request(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let length = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|n| n.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}
fn client(origin: &str) -> Client {
    Client::new(origin)
        .unwrap()
        .authenticated("synthetic-local-test".into())
}
async fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}
async fn response(socket: &mut TcpStream, status: &str, content: &str, body: &str) {
    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
}
const RELATIVE: &str = "/api/v2/agent/threads/thread/events?turn_id=turn&after_sequence=0";
#[tokio::test]
async fn consumer_drop_closes_v2_idle_body_without_waiting_for_timeout() {
    let (listener, url) = listener().await;
    let (ready, started) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let req = request(&mut socket).await;
        assert!(req.starts_with("GET "));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        ready.send(()).unwrap();
        let mut buf = [0; 1];
        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buf))
            .await
            .unwrap()
            .unwrap()
    });
    let (tx, rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    let c = client(&url);
    let task =
        tokio::spawn(async move { c.watch_turn("thread", "turn", RELATIVE, tx, cancel).await });
    started.await.unwrap();
    drop(rx);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(server.await.unwrap(), 0);
}
#[tokio::test]
async fn consumer_drop_cancels_pending_response_headers() {
    let (listener, url) = listener().await;
    let (ready, started) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        ready.send(()).unwrap();
        let mut buf = [0; 1];
        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buf))
            .await
            .unwrap()
            .unwrap()
    });
    let (tx, rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    let c = client(&url);
    let task =
        tokio::spawn(async move { c.watch_turn("thread", "turn", RELATIVE, tx, cancel).await });
    started.await.unwrap();
    drop(rx);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(server.await.unwrap(), 0);
}
#[tokio::test]
async fn consumer_drop_interrupts_retry_after_backoff() {
    let (listener, url) = listener().await;
    let (ready, started) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 503 Unavailable\r\nRetry-After: 3600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        ready.send(()).unwrap();
    });
    let (tx, rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    let c = client(&url);
    let task =
        tokio::spawn(async move { c.watch_turn("thread", "turn", RELATIVE, tx, cancel).await });
    started.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(rx);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
}
fn event(sequence: u64, kind: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({"schema_version":"agent-event/v2","thread_id":"thread","sequence":sequence,"event":{"turn_id":"turn","payload":{"event_type":kind,"data":{"text":"hello"}}}})
    )
}
#[tokio::test]
async fn reconnect_is_get_only_resumes_sequence_and_suppresses_duplicate_events() {
    let (listener, url) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let first = request(&mut socket).await;
        assert!(first.starts_with("GET ") && first.contains("after_sequence=0"));
        response(
            &mut socket,
            "200 OK",
            "text/event-stream",
            &event(1, "text_delta"),
        )
        .await;
        drop(socket);
        let (mut socket, _) = listener.accept().await.unwrap();
        let next = request(&mut socket).await;
        assert!(next.starts_with("GET ") && next.contains("after_sequence=1"));
        response(
            &mut socket,
            "200 OK",
            "text/event-stream",
            &(event(1, "text_delta") + &event(2, "run_finished")),
        )
        .await;
    });
    let (tx, mut rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    let c = client(&url);
    let task =
        tokio::spawn(async move { c.watch_turn("thread", "turn", RELATIVE, tx, cancel).await });
    let mut sequences = vec![];
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        sequences.push(event.unwrap().sequence)
    }
    task.await.unwrap().unwrap();
    server.await.unwrap();
    assert_eq!(sequences, vec![1, 2]);
}
#[tokio::test]
async fn cancellation_interrupts_backpressured_eof_flush() {
    let (listener, url) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(request(&mut socket).await.starts_with("POST "));
        response(
            &mut socket,
            "200 OK",
            "text/event-stream",
            "event: text_delta\ndata: {\"data\":\"tail\"}",
        )
        .await;
    });
    let (tx, mut rx) = mpsc::channel(1);
    tx.send(Ok(ai_platform_client::sdk_stream::SdkEvent {
        event_type: "occupied".into(),
        data: json!({}),
        timestamp: None,
    }))
    .await
    .unwrap();
    let (cancel_tx, cancel) = watch::channel(false);
    let c = client(&url);
    let task = tokio::spawn(async move {
        c.assistant_stream(json!({"message":"hello"}), tx, cancel)
            .await
    });
    server.await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(rx.recv().await.unwrap().unwrap().event_type, "occupied");
    assert!(rx.recv().await.is_none());
}
#[tokio::test]
async fn wrong_content_type_rejected_without_retrying_post() {
    let (listener, url) = listener().await;
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        seen.fetch_add(1, Ordering::SeqCst);
        request(&mut socket).await;
        response(&mut socket, "200 OK", "application/json", "{}").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let (tx, _rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    assert!(matches!(
        client(&url)
            .assistant_stream(json!({"message":"hello"}), tx, cancel)
            .await,
        Err(Error::Response)
    ));
    server.await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[test]
fn frame_and_unterminated_line_limits_reject_oversize_and_keep_fragmented_utf8() {
    let oversized = vec![b'x'; 1_048_577];
    assert!(Decoder::default().push(&oversized).is_err());
    assert!(SdkDecoder::default().push(&oversized).is_err());
    let mut plain = Decoder::default();
    let mut sdk = SdkDecoder::default();
    let data = "event: text_delta\ndata: {\"data\":\"你好\"}\n\n";
    let mut a = vec![];
    let mut b = vec![];
    for byte in data.as_bytes() {
        a.extend(plain.push(&[*byte]).unwrap());
        b.extend(sdk.push(&[*byte]).unwrap());
    }
    assert_eq!(a.len(), 1);
    assert_eq!(b[0].text(), "你好");
    let mut decoder = SdkDecoder::default();
    let line = format!("data: {}\n", "x".repeat(1024));
    let mut failed = false;
    for _ in 0..1100 {
        if decoder.push(line.as_bytes()).is_err() {
            failed = true;
            break;
        }
    }
    assert!(failed);
}

#[tokio::test]
async fn already_dropped_consumer_never_admits_a_turn() {
    let (listener, url) = listener().await;
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    let (_cancel, cancel) = watch::channel(false);
    client(&url)
        .assistant_stream(json!({"message":"hello"}), tx, cancel)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn stream_admission_503_never_retries_mutation() {
    let (listener, url) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(request(&mut socket).await.starts_with("POST "));
        response(&mut socket, "503 Unavailable", "application/json", "{}").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let (tx, _rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    assert_eq!(
        client(&url)
            .assistant_stream(json!({"message":"hello"}), tx, cancel)
            .await
            .unwrap_err()
            .status(),
        Some(503)
    );
    server.await.unwrap();
}

#[test]
fn typed_stream_limits_preserve_defaults_and_validate_bounds() {
    use ai_platform_client::{
        StreamLimits,
        sdk::{AssistantClient, AuthConfig},
    };
    let defaults = StreamLimits::default();
    assert_eq!(defaults.header_timeout(), Duration::from_secs(30));
    assert_eq!(defaults.idle_timeout(), Duration::from_secs(90));
    assert!(StreamLimits::new(Duration::ZERO, Duration::from_secs(1)).is_err());
    assert!(StreamLimits::new(Duration::from_secs(1), Duration::from_secs(86401)).is_err());
    let long = StreamLimits::new(Duration::from_secs(600), Duration::from_secs(3600)).unwrap();
    assert_eq!(
        client("http://127.0.0.1:8080")
            .with_stream_limits(long)
            .stream_limits(),
        long
    );
    AssistantClient::new_with_stream_limits(AuthConfig::new("synthetic-key"), long).unwrap();
}
#[tokio::test]
async fn configured_stream_header_timeout_marks_uncertain_admission_without_retry() {
    let (listener, url) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(request(&mut socket).await.starts_with("POST "));
        let mut b = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut b))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let limits =
        ai_platform_client::StreamLimits::new(Duration::from_millis(50), Duration::from_secs(1))
            .unwrap();
    let (tx, _rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    assert!(matches!(
        client(&url)
            .with_stream_limits(limits)
            .assistant_stream(json!({"message":"hello"}), tx, cancel)
            .await,
        Err(Error::UnknownOutcome)
    ));
    server.await.unwrap();
}
#[tokio::test]
async fn configured_idle_timeout_closes_body_without_retrying_turn() {
    let (listener, url) = listener().await;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        let mut b = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut b))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    });
    let limits =
        ai_platform_client::StreamLimits::new(Duration::from_secs(1), Duration::from_millis(50))
            .unwrap();
    let (tx, _rx) = mpsc::channel(1);
    let (_cancel, cancel) = watch::channel(false);
    assert!(matches!(
        client(&url)
            .with_stream_limits(limits)
            .assistant_stream(json!({"message":"hello"}), tx, cancel)
            .await,
        Err(Error::Transport)
    ));
    server.await.unwrap();
}

#[test]
fn all_client_transports_explicitly_disable_library_level_retries() {
    // reqwest 0.13 retries protocol NACKs by default. Protect the single-admission
    // invariant at each independently configured client, including MCP tools.
    for (name, source) in [
        ("core", include_str!("../src/lib.rs")),
        ("assistant", include_str!("../src/assistant_sdk.rs")),
        ("thread SDK", include_str!("../src/sdk_threads.rs")),
        ("event observer", include_str!("../src/events.rs")),
        ("MCP", include_str!("../src/mcp.rs")),
        (
            "publication example",
            include_str!("../examples/agent_runtime.rs"),
        ),
    ] {
        let builders = source.matches("reqwest::Client::builder()").count();
        assert!(builders > 0, "{name}");
        assert_eq!(
            builders,
            source.matches(".retry(reqwest::retry::never())").count(),
            "{name} must explicitly disable implicit retries"
        );
    }
}
