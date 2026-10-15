//! GET-only Runtime event observation. Transport liveness never renews the
//! signed execution lease, and command requests retain their separate deadline.
use super::AgentError;
use reqwest::{Client, Response, header::HeaderMap};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
// Runtime emits an SSE keep-alive comment every 15 seconds, including approval
// waits. Allow missed heartbeats without treating an active stream as expired.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone)]
pub(super) struct EventTransport {
    client: Client,
    header_timeout: Duration,
}

impl EventTransport {
    pub(super) fn new() -> Result<Self, AgentError> {
        Self::with_timeouts(CONNECT_TIMEOUT, HEADER_TIMEOUT, IDLE_TIMEOUT)
    }

    fn with_timeouts(
        connect: Duration,
        header_timeout: Duration,
        idle: Duration,
    ) -> Result<Self, AgentError> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect)
            .read_timeout(idle)
            // Deliberately no whole-response timeout on event observation.
            .pool_max_idle_per_host(20)
            .build()
            .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_UNAVAILABLE"))?;
        Ok(Self {
            client,
            header_timeout,
        })
    }

    pub(super) async fn open(
        &self,
        url: String,
        headers: HeaderMap,
        after: i64,
        limit: i64,
    ) -> Result<Response, AgentError> {
        // Scope/auth/correlation were independently built by the control plane.
        // Limit this timeout to send()/response headers; body reads use idle.
        tokio::time::timeout(
            self.header_timeout,
            self.client
                .get(url)
                .headers(headers)
                .query(&[
                    ("after_sequence", after.max(0)),
                    ("limit", limit.clamp(1, 1000)),
                ])
                .send(),
        )
        .await
        .map_err(|_| stream_error())?
        .map_err(|_| stream_error())
    }
}

fn stream_error() -> AgentError {
    AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        task::JoinHandle,
        time::{sleep, timeout},
    };

    const SSE_HEADERS: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";

    struct Server(JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    async fn serve<F, Fut>(handler: F) -> (String, oneshot::Receiver<String>, Server)
    where
        F: FnOnce(TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/events", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
                assert!(request.len() < 16 * 1024, "bounded fixture headers");
            }
            let _ = tx.send(String::from_utf8(request).unwrap());
            handler(stream).await;
        });
        (url, rx, Server(task))
    }

    async fn chunk(stream: &mut TcpStream, payload: &str) {
        stream
            .write_all(format!("{:x}\r\n{payload}\r\n", payload.len()).as_bytes())
            .await
            .unwrap();
    }

    fn transport(headers: Duration, idle: Duration) -> EventTransport {
        EventTransport::with_timeouts(Duration::from_secs(1), headers, idle).unwrap()
    }

    #[test]
    fn production_budgets_remain_finite_and_distinct() {
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(HEADER_TIMEOUT, Duration::from_secs(30));
        assert_eq!(IDLE_TIMEOUT, Duration::from_secs(45));
        assert!(IDLE_TIMEOUT > Duration::from_secs(15));
        EventTransport::new().unwrap();
    }

    #[tokio::test]
    async fn heartbeats_keep_body_alive_beyond_header_and_idle_budgets() {
        let (url, request, _server) = serve(|mut stream| async move {
            stream.write_all(SSE_HEADERS).await.unwrap();
            for _ in 0..8 {
                chunk(&mut stream, ": keep-alive\n\n").await;
                sleep(Duration::from_millis(75)).await;
            }
            chunk(&mut stream, "data: {\"sequence\":18}\n\n").await;
            stream.write_all(b"0\r\n\r\n").await.unwrap();
        })
        .await;
        let client = transport(Duration::from_millis(250), Duration::from_millis(250));
        let mut response = client.open(url, HeaderMap::new(), 17, 100).await.unwrap();
        let body = timeout(Duration::from_secs(3), async {
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.unwrap() {
                body.extend_from_slice(&chunk);
            }
            String::from_utf8(body).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(body.matches(": keep-alive\n\n").count(), 8);
        assert!(body.contains("data: {\"sequence\":18}"));
        assert!(
            request
                .await
                .unwrap()
                .starts_with("GET /events?after_sequence=17&limit=100 ")
        );
    }

    #[tokio::test]
    async fn headers_have_a_total_deadline_before_body_consumption() {
        let (url, _request, _server) = serve(|stream| async move {
            sleep(Duration::from_secs(3)).await;
            drop(stream);
        })
        .await;
        let client = transport(Duration::from_millis(50), Duration::from_secs(2));
        let error = timeout(
            Duration::from_millis(500),
            client.open(url, HeaderMap::new(), 0, 100),
        )
        .await
        .expect("header deadline must precede idle deadline")
        .unwrap_err();
        assert_eq!(error.status, 503);
        assert_eq!(
            error.detail["code"],
            "AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED"
        );
    }

    #[tokio::test]
    async fn a_silent_body_expires_after_its_last_heartbeat() {
        let (url, _request, _server) = serve(|mut stream| async move {
            stream.write_all(SSE_HEADERS).await.unwrap();
            chunk(&mut stream, ": keep-alive\n\n").await;
            sleep(Duration::from_secs(3)).await;
        })
        .await;
        let client = transport(Duration::from_secs(1), Duration::from_millis(60));
        let mut response = client.open(url, HeaderMap::new(), 0, 100).await.unwrap();
        assert_eq!(
            response.chunk().await.unwrap().unwrap().as_ref(),
            b": keep-alive\n\n"
        );
        let error = timeout(Duration::from_millis(600), response.chunk())
            .await
            .expect("idle deadline must end a silent read")
            .unwrap_err();
        assert!(error.is_timeout());
    }

    #[tokio::test]
    async fn admitted_headers_and_resume_cursor_are_forwarded_unchanged() {
        for (after, limit, expected) in [
            (432, 7, "after_sequence=432&limit=7"),
            (-2, 9_999, "after_sequence=0&limit=1000"),
            (1, 0, "after_sequence=1&limit=1"),
        ] {
            let (url, request, _server) = serve(|mut stream| async move {
                stream.write_all(SSE_HEADERS).await.unwrap();
                stream.write_all(b"0\r\n\r\n").await.unwrap();
            })
            .await;
            let mut headers = HeaderMap::new();
            for (name, value) in [
                ("x-ai-platform-internal-token", "fixture-service-token"),
                ("x-ai-tenant-id", "fixture-tenant"),
                ("x-ai-user-id", "fixture-user"),
                ("x-ai-session-id", "fixture-session"),
                ("x-ai-run-id", "fixture-run"),
                ("x-ai-turn-id", "fixture-run"),
                ("x-request-id", "fixture-request"),
                (
                    "traceparent",
                    "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
                ),
            ] {
                headers.insert(name, value.parse().unwrap());
            }
            let response = EventTransport::new()
                .unwrap()
                .open(url, headers.clone(), after, limit)
                .await
                .unwrap();
            assert!(response.status().is_success());
            let request = request.await.unwrap();
            assert!(request.starts_with(&format!("GET /events?{expected} ")));
            for (name, value) in &headers {
                assert!(request.contains(&format!("{}: {}\r\n", name, value.to_str().unwrap())));
            }
        }
    }

    #[tokio::test]
    async fn redirects_are_returned_without_following() {
        let (url, _request, _server) = serve(|mut stream| async move {
            stream.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /must-not-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        }).await;
        let response = EventTransport::new()
            .unwrap()
            .open(url, HeaderMap::new(), 0, 100)
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    }

    #[tokio::test]
    async fn pending_header_wait_remains_cancellable() {
        let (url, request, _server) = serve(|stream| async move {
            sleep(Duration::from_secs(3)).await;
            drop(stream);
        })
        .await;
        let client = EventTransport::new().unwrap();
        let task = tokio::spawn(async move { client.open(url, HeaderMap::new(), 0, 100).await });
        timeout(Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        // Reconnection is owned by the existing scoped watcher, never by this
        // one-shot transport future. EventReceiver drop/cleanup is unchanged.
    }
}
