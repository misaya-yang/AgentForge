use super::*;
use std::io::{Read, Write};
fn response_fixture(
    status: u16,
    extra: &str,
    payload: &[u8],
    complete: bool,
) -> (Bridge, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let extra = extra.to_owned();
    let payload = payload.to_vec();
    let join = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        let (mut socket, _) = loop {
            match listener.accept() {
                Ok(v) => break v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "request not dispatched");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("{e}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut head = Vec::new();
        let mut byte = [0];
        while !head.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
            assert!(head.len() < 16384);
        }
        let head = String::from_utf8(head).unwrap();
        let length = head
            .lines()
            .find_map(|l| {
                l.to_lowercase()
                    .strip_prefix("content-length:")
                    .map(|x| x.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({}));
        write!(
            socket,
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n{extra}\r\n",
            if complete {
                payload.len()
            } else {
                payload.len() + 1_000_000
            }
        )
        .unwrap();
        socket.write_all(&payload).unwrap();
        socket.flush().unwrap();
        if !complete {
            // An unconsumed status body must not stay owned by the retained error.
            match socket.read(&mut byte) {
                Ok(0) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) => {}
                other => panic!("response connection retained: {other:?}"),
            }
        }
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    });
    (
        Bridge {
            client: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            origin: Some(origin),
        },
        join,
    )
}
#[tokio::test]
async fn http_error_body_is_released_before_retained_error() {
    for status in [400, 401, 403, 409, 429, 500, 503] {
        let (bridge, join) = response_fixture(status, "", b"secret-shaped-error-body", false);
        let retained = bridge
            .request("capabilities", "admit", &"a".repeat(64), json!({}))
            .await
            .unwrap_err();
        tokio::task::spawn_blocking(move || join.join())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.to_string(), "sdk_bridge_request_failed");
        assert!(!retained.to_string().contains("secret-shaped"));
    }
}
#[tokio::test]
async fn redirects_do_not_follow_or_retain_response_body() {
    for status in [301, 302, 303, 307, 308] {
        for location in [
            "Location: https://elsewhere.invalid/secret-shaped-target\r\n",
            "Location: file:///private\r\n",
            "",
        ] {
            let (bridge, join) =
                response_fixture(status, location, b"secret-shaped-redirect-body", false);
            let retained = bridge
                .request("knowledge", "admit", &"b".repeat(64), json!({}))
                .await
                .unwrap_err();
            tokio::task::spawn_blocking(move || join.join())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.to_string(), "sdk_bridge_request_failed");
        }
    }
}
#[tokio::test]
async fn success_and_malformed_bodies_use_actual_bounded_parser() {
    for raw in [br#"{"authorized":true}"#.as_slice(), b"not-json", b"\xff"] {
        let (bridge, join) = response_fixture(200, "", raw, true);
        let result = bridge
            .request("knowledge", "admit", &"b".repeat(64), json!({}))
            .await;
        if raw.starts_with(b"{") {
            assert_eq!(result.unwrap(), json!({"authorized":true}));
        } else {
            assert!(result.is_err());
        }
        tokio::task::spawn_blocking(move || join.join())
            .await
            .unwrap()
            .unwrap();
    }
    let (bridge, join) = response_fixture(200, "", &vec![b'x'; 1_048_577], true);
    assert_eq!(
        bridge
            .request("capabilities", "admit", &"a".repeat(64), json!({}))
            .await
            .unwrap_err()
            .to_string(),
        "sdk_bridge_output_limit"
    );
    tokio::task::spawn_blocking(move || join.join())
        .await
        .unwrap()
        .unwrap();
}
