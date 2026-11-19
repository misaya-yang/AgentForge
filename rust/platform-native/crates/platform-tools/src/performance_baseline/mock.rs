//! Finite authenticated deterministic OpenAI-compatible upstream. No provider I/O.
use super::{Result, config::MockConfig, ensure, evidence, sha};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
#[derive(Clone, Serialize)]
pub struct Observation {
    pub correlation: String,
    pub trace_id: Option<String>,
    pub route: String,
    pub request_sha256: Option<String>,
    pub elapsed_ms: Option<f64>,
    pub response_write_ms: Option<f64>,
    pub response_bytes: Option<usize>,
    pub outcome: String,
}
pub struct Handle {
    task: tokio::task::JoinHandle<()>,
    rows: Arc<Mutex<Vec<Observation>>>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Handle {
    pub fn observations(&self) -> Vec<Observation> {
        self.rows.lock().map(|r| r.clone()).unwrap_or_default()
    }
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |v, (a, b)| v | (a ^ b)) == 0
}
pub fn marker(value: &Value, prefix: &str) -> Option<String> {
    let raw = serde_json::to_string(value).ok()?;
    let at = raw.find(prefix)?;
    let result = raw[at..]
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_')))
        .next()?;
    (!result.is_empty() && result.len() <= 220).then(|| result.into())
}
pub fn response(path: &str, body: &Value, c: &MockConfig) -> Result<Value> {
    match path {
        "/v1/chat/completions" => {
            ensure(
                body.get("messages").is_some_and(Value::is_array)
                    && body.get("stream") != Some(&Value::Bool(true)),
                "mock_nonstream_messages_required",
            )?;
            Ok(
                json!({"id":"chatcmpl-deterministic-performance","object":"chat.completion","created":0,"model":c.model,"choices":[{"index":0,"message":{"role":"assistant","content":c.response_text},"finish_reason":"stop"}],"usage":{"prompt_tokens":32,"completion_tokens":8,"total_tokens":40}}),
            )
        }
        "/v1/embeddings" => {
            let inputs = match &body["input"] {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a
                    .iter()
                    .map(|v| v.as_str().ok_or("mock_embedding_text_required"))
                    .collect::<std::result::Result<Vec<_>, _>>()?,
                _ => return Err("mock_embedding_input".into()),
            };
            ensure(
                (1..=32).contains(&inputs.len()),
                "mock_embedding_batch_budget",
            )?;
            let data = inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    let hash = sha(input.as_bytes());
                    let mut vector = (0..c.embedding_dimensions)
                        .map(|i| {
                            let n = u8::from_str_radix(&hash[2 * (i % 32)..2 * (i % 32) + 2], 16)
                                .unwrap_or(0);
                            (n as f64 - 127.5) / 128.0
                        })
                        .collect::<Vec<_>>();
                    let norm = vector.iter().map(|x| x * x).sum::<f64>().sqrt().max(1e-12);
                    for n in &mut vector {
                        *n /= norm;
                    }
                    json!({"index":index,"object":"embedding","embedding":vector})
                })
                .collect::<Vec<_>>();
            Ok(
                json!({"object":"list","data":data,"model":c.model,"usage":{"prompt_tokens":32*inputs.len(),"total_tokens":32*inputs.len()}}),
            )
        }
        "/v1/rerank" => {
            let docs = body["documents"]
                .as_array()
                .ok_or("mock_rerank_documents")?;
            ensure((1..=64).contains(&docs.len()), "mock_rerank_budget")?;
            Ok(
                json!({"id":"deterministic-rerank","results":docs.iter().enumerate().map(|(i,_)|json!({"index":i,"relevance_score":1.0/(i+1)as f64})).collect::<Vec<_>>(),"usage":{"total_tokens":32}}),
            )
        }
        _ => Err("mock_route_rejected".into()),
    }
}
async fn request(stream: &mut TcpStream) -> Result<(String, BTreeMap<String, String>, Vec<u8>)> {
    let mut bytes = Vec::new();
    let end = loop {
        ensure(bytes.len() < 8192, "mock_header_budget")?;
        let mut b = [0u8; 1024];
        let n = stream.read(&mut b).await.map_err(|_| "mock_read")?;
        ensure(n > 0, "mock_header_eof")?;
        bytes.extend_from_slice(&b[..n]);
        if let Some(n) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            break n + 4;
        }
    };
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| "mock_header_encoding")?;
    let mut lines = text.split("\r\n");
    let first = lines
        .next()
        .ok_or("mock_request_line")?
        .split_whitespace()
        .collect::<Vec<_>>();
    ensure(
        first.len() == 3 && first[0] == "POST" && first[2] == "HTTP/1.1",
        "mock_method",
    )?;
    let path = first[1].to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once(':').ok_or("mock_header_shape")?;
        ensure(
            headers
                .insert(k.to_ascii_lowercase(), v.trim().into())
                .is_none(),
            "mock_duplicate_header",
        )?;
    }
    ensure(
        !headers.contains_key("transfer-encoding") && !headers.contains_key("expect"),
        "mock_request_framing",
    )?;
    let size = headers
        .get("content-length")
        .and_then(|s: &String| s.parse::<usize>().ok())
        .filter(|n| *n <= 2 * 1024 * 1024)
        .ok_or("mock_content_length")?;
    let mut body = bytes[end..].to_vec();
    ensure(body.len() <= size, "mock_pipeline_refused")?;
    while body.len() < size {
        let mut b = [0u8; 4096];
        let n = stream
            .read(&mut b[..(size - body.len()).min(4096)])
            .await
            .map_err(|_| "mock_body_read")?;
        ensure(n > 0, "mock_body_eof")?;
        body.extend_from_slice(&b[..n]);
    }
    Ok((path, headers, body))
}
async fn serve(
    mut stream: TcpStream,
    c: &MockConfig,
    key: &str,
    prefix: &str,
) -> Result<Observation> {
    let start = Instant::now();
    let (path, headers, bytes) = request(&mut stream).await?;
    ensure(
        headers
            .get("authorization")
            .is_some_and(|v| equal(v.as_bytes(), format!("Bearer {key}").as_bytes())),
        "mock_auth_rejected",
    )?;
    let body: Value = serde_json::from_slice(&bytes).map_err(|_| "mock_json")?;
    let correlation = marker(&body, prefix).ok_or("mock_missing_current_run_correlation")?;
    let data = response(&path, &body, c)?;
    tokio::time::sleep(Duration::from_millis(c.delay_ms)).await;
    let bytes = serde_json::to_vec(&data).map_err(|_| "mock_encode")?;
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    let writing = Instant::now();
    stream
        .write_all(header.as_bytes())
        .await
        .map_err(|_| "mock_write")?;
    stream.write_all(&bytes).await.map_err(|_| "mock_write")?;
    stream.shutdown().await.map_err(|_| "mock_close")?;
    let trace_id = headers
        .get("traceparent")
        .and_then(|t| t.split('-').nth(1))
        .filter(|s| super::config::digest(&format!("{s}{s}")))
        .map(str::to_owned);
    Ok(Observation {
        correlation,
        trace_id,
        route: path,
        request_sha256: Some(sha(&serde_json::to_vec(&body).map_err(|_| "mock_encode")?)),
        elapsed_ms: Some(start.elapsed().as_secs_f64() * 1000.0),
        response_write_ms: Some(writing.elapsed().as_secs_f64() * 1000.0),
        response_bytes: Some(bytes.len()),
        outcome: "synthetic_upstream_response_written".into(),
    })
}
pub async fn start(
    c: MockConfig,
    run_id: &str,
    mut peers: Vec<std::net::IpAddr>,
) -> Result<Handle> {
    let origin = super::config::mock_origin(&c.origin)?;
    let address: std::net::IpAddr = origin
        .host_str()
        .unwrap()
        .parse()
        .map_err(|_| "mock_bind_address")?;
    ensure(
        local_address(address),
        "mock_address_not_assigned_to_this_host",
    )?;
    peers.push(address);
    let listener = TcpListener::bind((address, origin.port().unwrap()))
        .await
        .map_err(|_| "preconfigured_mock_listener_unavailable")?;
    let key = evidence::token(&c.token_file)?;
    let rows = Arc::new(Mutex::new(Vec::new()));
    let captured = rows.clone();
    let prefix = format!("af-perf:{run_id}:");
    let task = tokio::spawn(async move {
        for _ in 0..c.max_requests {
            let Ok((stream, peer)) = listener.accept().await else {
                break;
            };
            if !peers.contains(&peer.ip()) {
                continue;
            }
            let result =
                tokio::time::timeout(Duration::from_secs(10), serve(stream, &c, &key, &prefix))
                    .await;
            let row = match result {
                Ok(Ok(row)) => row,
                _ => Observation {
                    correlation: "unmatched_or_failed".into(),
                    trace_id: None,
                    route: "unavailable".into(),
                    request_sha256: None,
                    elapsed_ms: None,
                    response_write_ms: None,
                    response_bytes: None,
                    outcome: "mock_request_failed_or_deadline".into(),
                },
            };
            if let Ok(mut rows) = captured.lock() {
                rows.push(row);
            }
        }
    });
    Ok(Handle { task, rows })
}

fn local_address(ip: std::net::IpAddr) -> bool {
    #[cfg(unix)]
    unsafe {
        let mut head = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return false;
        }
        let mut cursor = head;
        let mut found = false;
        while !cursor.is_null() {
            let item = &*cursor;
            if !item.ifa_addr.is_null() && (*item.ifa_addr).sa_family as i32 == libc::AF_INET {
                let addr = &*(item.ifa_addr as *const libc::sockaddr_in);
                if std::net::IpAddr::V4(std::net::Ipv4Addr::from(
                    addr.sin_addr.s_addr.to_ne_bytes(),
                )) == ip
                {
                    found = true;
                }
            }
            cursor = item.ifa_next;
        }
        libc::freeifaddrs(head);
        found
    }
    #[cfg(not(unix))]
    {
        let _ = ip;
        false
    }
}
