//! Real HTTP decoding into typed problems followed by production admission rules.
//! Synthetic loopback only; no provider operation or mutation replay is performed.
use ai_platform_client::{ApiRequest, Client, Error};
use reqwest::Method;
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
async fn read_request(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        assert!(bytes.len() < 65536);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let length = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }
}
async fn fixture(status: u16, path: &'static str) -> (Client, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(&format!("http://{}", listener.local_addr().unwrap()))
        .unwrap()
        .authenticated("synthetic-token".into())
        .with_transport_options(Duration::from_secs(2), 3)
        .unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(read_request(&mut socket).await.starts_with(path));
        let body=json!({"type":"urn:agentforge:problem:test","title":"private-marker","status":status,"detail":{"credential":"private-marker"},"instance":"https://private-marker.invalid/request"}).to_string();
        socket.write_all(format!("HTTP/1.1 {status} Failure\r\nContent-Type: application/problem+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_err(),
            "mutation was automatically sent again"
        );
    });
    (client, server)
}
#[tokio::test]
async fn real_problem_mutation_status_and_error_output_preserve_privacy_without_replay() {
    for status in [401, 404, 408, 500, 503] {
        let (client, server) = fixture(status, "PATCH /api/v1/example ").await;
        let error = client
            .execute(ApiRequest::mutation(
                Method::PATCH,
                &["api", "v1", "example"],
                Some(json!({"state":"x"})),
            ))
            .await
            .unwrap_err();
        assert!(matches!(&error, Error::Problem(_)));
        assert_eq!(error.status(), Some(status));
        assert!(!format!("{error:?} {error}").contains("private-marker"));
        server.await.unwrap();
    }
}
#[tokio::test]
async fn real_problem_async_image_admission_keeps_known_rejections_or_unknown_effect() {
    for status in [401, 404, 408, 500, 503] {
        let (client, server) =
            fixture(status, "POST /api/v1/assistant/generate-image-async ").await;
        let error = client
            .assistant_image(
                json!({"prompt":"synthetic input","model_id":"synthetic-model"}),
                true,
            )
            .await
            .unwrap_err();
        if [401, 404].contains(&status) {
            assert_eq!(error.status(), Some(status));
            assert!(matches!(&error, Error::Problem(_)));
        } else {
            assert!(matches!(&error, Error::UnknownOutcome));
            assert!(error.to_string().contains("do not automatically resubmit"));
        }
        assert!(!format!("{error:?} {error}").contains("private-marker"));
        server.await.unwrap();
    }
}
