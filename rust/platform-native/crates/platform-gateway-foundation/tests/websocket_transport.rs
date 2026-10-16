use ai_platform_gateway_foundation::websocket_transport::{WebSocketConnector, WebSocketError};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
#[tokio::test]
async fn json_socket_uses_real_duplex_transport_with_control_frames_and_binary_json() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(socket).await.unwrap();
        let message = socket.next().await.unwrap().unwrap();
        assert_eq!(
            message.to_text().unwrap(),
            r#"{"type": "execute", "data": "\u4e2d\ud83d\ude00"}"#
        );
        socket.send(Message::Ping(vec![1, 2].into())).await.unwrap();
        socket
            .send(Message::Text(r#"{"type":"progress"}"#.into()))
            .await
            .unwrap();
        let mut bytes = vec![0xff, 0xfe];
        for unit in r#"{"type":"executed"}"#.encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        socket.send(Message::Binary(bytes.into())).await.unwrap();
        loop {
            match socket.next().await {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("loopback socket: {error}"),
            }
        }
    });
    let connector = WebSocketConnector::new(&format!("{base}/"), "/ws");
    assert!(connector.health_check());
    let mut socket = connector.websocket().await.unwrap();
    socket
        .send_json(&json!({"type":"execute","data":"中😀"}))
        .await
        .unwrap();
    assert_eq!(
        socket.recv_json().await.unwrap(),
        Some(json!({"type":"progress"}))
    );
    assert_eq!(
        socket.recv_json().await.unwrap(),
        Some(json!({"type":"executed"}))
    );
    socket.close().await.unwrap();
    server.await.unwrap();
}
#[tokio::test]
async fn malformed_json_is_typed_and_http_request_stays_unsupported() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connector =
        WebSocketConnector::new(&format!("ws://{}", listener.local_addr().unwrap()), "/ws");
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(socket).await.unwrap();
        socket.send(Message::Text("invalid".into())).await.unwrap();
    });
    let mut socket = connector.websocket().await.unwrap();
    assert_eq!(
        socket.recv_json().await.unwrap_err(),
        WebSocketError::InvalidJson
    );
    assert_eq!(
        connector.request("GET", "/", &json!({})).await.unwrap_err(),
        WebSocketError::UnsupportedRequest
    );
    server.await.unwrap();
}
