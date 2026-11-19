use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
async fn fixture(refuse: bool) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(vec![]));
    let captured = seen.clone();
    let task = tokio::spawn(async move {
        for _ in 0..if refuse { 1 } else { 3 } {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = vec![];
            let mut buf = [0; 1024];
            let split;
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buf[..n]);
                assert!(raw.len() < 32768);
                if let Some(at) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
                    let h = String::from_utf8_lossy(&raw[..at]).to_ascii_lowercase();
                    let length = h
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    if raw.len() >= at + 4 + length {
                        split = at;
                        break;
                    }
                }
            }
            let headers = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
            let payload: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
            let method = payload["method"].as_str().unwrap();
            assert!(headers.contains("mcp-protocol-version: 2026-07-28"));
            assert!(headers.contains(&format!("mcp-method: {method}")));
            assert!(!headers.contains("mcp-session-id"));
            assert_eq!(
                payload["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                "2026-07-28"
            );
            let result = match method {
                "server/discover" => {
                    json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}})
                }
                "tools/list" => {
                    json!({"resultType":"complete","tools":[{"name":"weather","inputSchema":{"type":"object","properties":{"region":{"type":"string","x-mcp-header":"Region"}}}}]})
                }
                "tools/call" => {
                    assert!(headers.contains("mcp-name: weather"));
                    assert!(headers.contains("mcp-param-region: eu"));
                    assert_eq!(payload["params"]["arguments"], json!({"region":"eu"}));
                    json!({"resultType":"complete","content":[]})
                }
                _ => panic!("legacy fallback or unexpected RPC"),
            };
            captured.lock().unwrap().push(payload.clone());
            let value = if refuse {
                json!({"jsonrpc":"2.0","id":payload["id"],"error":{"code":-32022,"data":{"supported":["2025-11-25"]}}})
            } else {
                json!({"jsonrpc":"2.0","id":payload["id"],"result":result})
            };
            let body = serde_json::to_vec(&value).unwrap();
            let status = if refuse { "400 Bad Request" } else { "200 OK" };
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    (format!("http://{address}/mcp"), seen, task)
}
#[tokio::test]
async fn actual_modern_http_discovery_list_call_has_per_request_meta_and_mirrors() {
    let (url, seen, task) = fixture(false).await;
    let mut client = McpClient::connect_with_revision(
        "fixture".into(),
        TransportConfig::Http { url, api_key: None },
        "2026-07-28",
    )
    .await
    .unwrap();
    let result = client
        .call_tool("weather", json!({"region":"eu"}))
        .await
        .unwrap();
    assert_eq!(result["resultType"], "complete");
    task.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn known_modern_mismatch_is_one_discovery_not_legacy_fallback_or_call_replay() {
    let (url, seen, task) = fixture(true).await;
    let result = McpClient::connect_with_revision(
        "fixture".into(),
        TransportConfig::Http { url, api_key: None },
        "2026-07-28",
    )
    .await;
    assert!(matches!(
        result,
        Err(Error::Contract("MCP_PROTOCOL_DOWNGRADE_REJECTED"))
    ));
    task.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}
