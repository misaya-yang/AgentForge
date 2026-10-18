use crate::workload_identity::WorkloadSend;
use crate::{
    contract::Launch,
    emit,
    tools::{Bridge, descriptors},
    usage,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{io::Write, path::Path, time::Duration};

pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let temp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)?;
    f.write_all(&serde_json::to_vec(value)?)?;
    f.sync_all()?;
    std::fs::rename(temp, path)?;
    std::fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
// Native history is distinct, but pinned OpenAI SQLiteSession items are already
// Responses input wire objects and can be imported without resetting context.
// Source: openai/openai-agents-python v0.23.1 src/agents/memory/sqlite_session.py.
const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
const MAX_HISTORY_ITEMS: usize = 10000;
fn history(l: &Launch, state: &Path) -> Result<Vec<Value>> {
    let target = state.join("native-history.json");
    if target.exists() {
        use std::io::Read;
        ensure!(
            !std::fs::symlink_metadata(&target)?.file_type().is_symlink(),
            "sdk_state_invalid"
        );
        let mut raw = Vec::new();
        std::fs::File::open(target)?
            .take((MAX_HISTORY_BYTES + 1) as u64)
            .read_to_end(&mut raw)?;
        ensure!(raw.len() <= MAX_HISTORY_BYTES, "sdk_history_limit");
        let v: Value = serde_json::from_slice(&raw)?;
        ensure!(
            v["schema"] == "sdk-native-history/v1" && v["framework"] == l.framework,
            "sdk_session_profile_changed"
        );
        let items: Vec<Value> = serde_json::from_value(v["messages"].clone())?;
        ensure!(
            items.len() <= MAX_HISTORY_ITEMS && items.iter().all(Value::is_object),
            "sdk_history_invalid"
        );
        return Ok(items);
    }
    // Claude stores an opaque SDK session identifier, not Messages API history.
    ensure!(
        !state.join("claude-session.json").exists(),
        "sdk_session_native_migration_required"
    );
    let legacy = state.join("openai.sqlite");
    if !legacy.exists() {
        return Ok(vec![]);
    }
    ensure!(
        l.framework == "openai_agents"
            && !std::fs::symlink_metadata(&legacy)?.file_type().is_symlink(),
        "sdk_session_native_migration_required"
    );
    let connection = rusqlite::Connection::open_with_flags(
        &legacy,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(1))?;
    connection.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        MAX_HISTORY_BYTES as i32,
    );
    connection.pragma_update(None, "trusted_schema", false)?;
    // Refuse a substituted view or virtual table rather than executing arbitrary schema.
    let definition: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='agent_messages'",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        !definition.to_ascii_uppercase().contains("VIRTUAL"),
        "sdk_history_invalid"
    );
    let mut query = connection.prepare(
        "SELECT message_data FROM agent_messages WHERE session_id = ?1 ORDER BY id ASC LIMIT 10001",
    )?;
    let mut rows = query.query([l.scope()])?;
    let mut items = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        bytes = bytes
            .checked_add(raw.len())
            .ok_or_else(|| anyhow::anyhow!("sdk_history_limit"))?;
        ensure!(
            bytes <= MAX_HISTORY_BYTES && items.len() < MAX_HISTORY_ITEMS,
            "sdk_history_limit"
        );
        let item: Value = serde_json::from_str(&raw)?;
        ensure!(item.is_object(), "sdk_history_invalid");
        items.push(item);
    }
    // Never modify the SDK database or persist a partial import. run() publishes
    // native history only once the first continued provider turn succeeds.
    Ok(items)
}
async fn sse(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    body: Value,
    claude: bool,
    streamed: &mut String,
) -> Result<Value> {
    let mut req = client.post(url).json(&body);
    req = req.bearer_auth(key);
    let mut response = req.workload_send().await?;
    ensure!(
        response.status().is_success(),
        "sdk_provider_execution_failed"
    );
    let mut receipt: Option<Value> = None;
    let mut buffer = Vec::new();
    let mut total = 0usize;
    let mut result: Option<Value> = None;
    let mut content: Vec<Value> = vec![];
    let mut arguments = std::collections::HashMap::<usize, String>::new();
    let mut usage = json!({});
    let mut stop = Value::Null;
    while let Some(chunk) = response.chunk().await? {
        total += chunk.len();
        ensure!(total <= 4 * 1024 * 1024, "sdk_output_limit");
        buffer.extend_from_slice(&chunk);
        ensure!(buffer.len() <= 1048576, "sdk_event_too_large");
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line = buffer.drain(..=end).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim_start();
            if data == "[DONE]" {
                continue;
            }
            let v: Value = serde_json::from_str(data)?;
            let typ = v["type"].as_str().unwrap_or("");
            if typ == "sdk.model_receipt" {
                ensure!(
                    receipt.is_none()
                        && v["settled"] == true
                        && v["cost_nanos"].as_u64().is_some()
                        && v["call_id"].is_string()
                        && v["receipt_id"].is_string(),
                    "sdk_model_receipt_invalid"
                );
                receipt = Some(v);
                continue;
            }
            if !claude {
                match typ {
                    "response.output_text.delta" | "response.refusal.delta" => {
                        let text = v["delta"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("sdk_event_invalid"))?;
                        streamed.push_str(text);
                        emit(&json!({"event":"delta","text":text}))?;
                    }
                    "response.completed" => {
                        ensure!(result.is_none(), "sdk_duplicate_terminal");
                        result = Some(v["response"].clone());
                    }
                    "error" | "response.failed" | "response.incomplete" => {
                        anyhow::bail!("sdk_provider_execution_failed")
                    }
                    _ => {}
                }
            } else {
                match typ {
                    "message_start" => {
                        usage = v["message"]["usage"].clone();
                    }
                    "content_block_start" => {
                        let i = v["index"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("sdk_event_invalid"))?
                            as usize;
                        ensure!(i == content.len() && i < 128, "sdk_event_invalid");
                        content.push(v["content_block"].clone());
                    }
                    "content_block_delta" => {
                        let i = v["index"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("sdk_event_invalid"))?
                            as usize;
                        ensure!(i < content.len(), "sdk_event_invalid");
                        match v["delta"]["type"].as_str() {
                            Some("text_delta") => {
                                let text = v["delta"]["text"]
                                    .as_str()
                                    .ok_or_else(|| anyhow::anyhow!("sdk_event_invalid"))?;
                                let mut prior =
                                    content[i]["text"].as_str().unwrap_or("").to_owned();
                                prior.push_str(text);
                                content[i]["text"] = json!(prior);
                                streamed.push_str(text);
                                emit(&json!({"event":"delta","text":text}))?;
                            }
                            Some("input_json_delta") => {
                                arguments.entry(i).or_default().push_str(
                                    v["delta"]["partial_json"]
                                        .as_str()
                                        .ok_or_else(|| anyhow::anyhow!("sdk_event_invalid"))?,
                                );
                            }
                            Some("thinking_delta") => {
                                let mut prior =
                                    content[i]["thinking"].as_str().unwrap_or("").to_owned();
                                prior.push_str(v["delta"]["thinking"].as_str().unwrap_or(""));
                                content[i]["thinking"] = json!(prior);
                            }
                            Some("signature_delta") => {
                                content[i]["signature"] = v["delta"]["signature"].clone();
                            }
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        stop = v["delta"]["stop_reason"].clone();
                        if let Some(fields) = v["usage"].as_object() {
                            for (k, val) in fields {
                                usage[k] = val.clone();
                            }
                        }
                    }
                    "message_stop" => {
                        ensure!(result.is_none(), "sdk_duplicate_terminal");
                        for (i, a) in &arguments {
                            content[*i]["input"] = serde_json::from_str(a)?;
                        }
                        result = Some(json!({"content":content,"usage":usage,"stop_reason":stop}));
                    }
                    "error" => anyhow::bail!("sdk_provider_execution_failed"),
                    _ => {}
                }
            }
        }
    }
    ensure!(
        buffer.iter().all(u8::is_ascii_whitespace),
        "sdk_stream_incomplete"
    );
    let mut result = result.ok_or_else(|| anyhow::anyhow!("sdk_stream_incomplete"))?;
    let receipt = receipt.ok_or_else(|| anyhow::anyhow!("sdk_model_receipt_missing"))?;
    if let Some(expected) = body.get("call_id") {
        ensure!(receipt["call_id"] == *expected, "sdk_model_receipt_invalid");
    }
    if let Some(expected) = body.get("run_id") {
        ensure!(receipt["run_id"] == *expected, "sdk_model_receipt_invalid");
    }
    result["model_plane_receipt"] = receipt;
    Ok(result)
}
fn final_text(output: &[Value], claude: bool) -> String {
    let mut text = String::new();
    for item in output {
        if claude && item["type"] == "text" {
            text.push_str(item["text"].as_str().unwrap_or(""));
        }
        if !claude && item["type"] == "message" {
            if let Some(parts) = item["content"].as_array() {
                for part in parts {
                    if part["type"] == "output_text" {
                        text.push_str(part["text"].as_str().unwrap_or(""));
                    } else if part["type"] == "refusal" {
                        text.push_str(part["refusal"].as_str().unwrap_or(""));
                    }
                }
            }
        }
    }
    text
}

pub async fn run(l: Launch, plane: crate::model_plane::ModelPlane, state: &Path) -> Result<()> {
    let bridge = Bridge::new()?;
    bridge.admit(&l).await?;
    ensure_profile(&l, state)?;
    let mut messages = history(&l, state)?;
    messages.push(json!({"role":"user","content":l.input}));
    let claude = l.framework == "claude_agent_sdk";
    let tools = descriptors(&l);
    let ca = std::env::var_os("SDK_AGENT_GATEWAY_TLS_CA_FILE").map(std::path::PathBuf::from);
    let client = crate::workload_identity::internal_client_with_ca(
        &[&plane.url],
        Duration::from_secs(5),
        Duration::from_secs(l.timeout()),
        ca.as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let mut raw_usage = vec![];
    let mut receipts = vec![];
    let mut streamed = String::new();
    let mut seen = std::collections::HashSet::new();
    for call_index in 0..l.agent.max_turns {
        plane.remaining().map_err(anyhow::Error::msg)?;
        bridge.admit_execution(&l).await?;
        let body = if claude {
            json!({"model":l.agent.model,"system":l.agent.instructions,"messages":messages,"tools":tools,"max_tokens":16384,"stream":true})
        } else {
            json!({"model":l.agent.model,"instructions":l.agent.instructions,"input":messages,"tools":tools.iter().map(|t|json!({"type":"function","name":t["name"],"description":t["description"],"parameters":t["input_schema"],"strict":false})).collect::<Vec<_>>(),"stream":true,"store":false})
        };
        let call_id = format!("{}:{call_index}", plane.run_id);
        let envelope = json!({"run_id":plane.run_id,"call_id":call_id,"wire":if claude{"anthropic_messages"}else{"responses_v1"},"body":body});
        let result = tokio::time::timeout(
            plane.remaining().map_err(anyhow::Error::msg)?,
            sse(
                &client,
                &plane.url,
                &plane.lease,
                envelope,
                claude,
                &mut streamed,
            ),
        )
        .await??;
        ensure!(
            if claude {
                matches!(
                    result["stop_reason"].as_str(),
                    Some("end_turn" | "stop_sequence" | "tool_use")
                )
            } else {
                result["status"] == "completed"
            },
            "sdk_provider_execution_failed"
        );
        raw_usage.push(result["usage"].clone());
        receipts.push(result["model_plane_receipt"].clone());
        let output = result[if claude { "content" } else { "output" }]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("sdk_provider_execution_failed"))?;
        if claude {
            messages.push(json!({"role":"assistant","content":output}));
        } else {
            messages.extend(output.iter().cloned());
        }
        let final_text = final_text(output, claude);
        let mut returns = vec![];
        for item in output {
            if item["type"] == if claude { "tool_use" } else { "function_call" } {
                ensure!(
                    !claude || result["stop_reason"] == "tool_use",
                    "sdk_provider_execution_failed"
                );
                let id = item[if claude { "id" } else { "call_id" }]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("sdk_tool_call_identity_required"))?;
                ensure!(
                    seen.insert(id.to_owned()) && seen.len() <= 128,
                    "sdk_tool_call_identity_ambiguous"
                );
                let name = item["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("sdk_tool_not_allowed"))?;
                let args = if claude {
                    item["input"].clone()
                } else {
                    serde_json::from_str(
                        item["arguments"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("sdk_tool_arguments_invalid"))?,
                    )?
                };
                let value = bridge.invoke(&l, name, args, id).await?;
                returns.push(if claude{json!({"type":"tool_result","tool_use_id":id,"content":serde_json::to_string(&value)?})}else{json!({"type":"function_call_output","call_id":id,"output":serde_json::to_string(&value)?})});
            }
        }
        if returns.is_empty() {
            ensure!(
                if claude {
                    matches!(
                        result["stop_reason"].as_str(),
                        Some("end_turn" | "stop_sequence")
                    )
                } else {
                    result["status"] == "completed"
                },
                "sdk_provider_execution_failed"
            );
            bridge.admit_execution(&l).await?;
            atomic_json(
                &state.join("native-history.json"),
                &json!({"schema":"sdk-native-history/v1","framework":l.framework,"messages":messages}),
            )?;
            emit(
                &json!({"event":"completed","text":final_text,"streamed_text":if streamed.is_empty(){&final_text}else{&streamed},"usage":usage::with_model_receipts(&raw_usage,&receipts,claude).map_err(anyhow::Error::msg)?}),
            )?;
            return Ok(());
        }
        ensure!(
            !claude || result["stop_reason"] == "tool_use",
            "sdk_provider_execution_failed"
        );
        if claude {
            messages.push(json!({"role":"user","content":returns}));
        } else {
            messages.extend(returns);
        }
    }
    anyhow::bail!("sdk_max_turns_exceeded")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(l: &str) -> Launch {
        let mut v:Launch=serde_json::from_value(json!({"protocol":"ai-platform/sdk-agent/v1","framework":l,"service_id":"s","tenant_id":"t","user_id":"u","request_id":"r","session_id":"ss","agent":{"model":"fixture","instructions":"fixture","max_turns":2},"input":"hello","stream":true})).unwrap();
        v.validate().unwrap();
        v
    }
    #[test]
    fn native_history_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let l = fixture("openai_agents");
        assert!(history(&l, d.path()).unwrap().is_empty());
        let messages = json!([{"role":"user","content":"hello"}]);
        atomic_json(
            &d.path().join("native-history.json"),
            &json!({"schema":"sdk-native-history/v1","framework":l.framework,"messages":messages}),
        )
        .unwrap();
        assert_eq!(
            history(&l, d.path()).unwrap(),
            messages.as_array().unwrap().clone()
        );
        assert!(history(&fixture("claude_agent_sdk"), d.path()).is_err());
    }
    #[test]
    fn openai_sqlite_history_import_is_scoped_ordered_and_read_only() {
        let d = tempfile::tempdir().unwrap();
        let l = fixture("openai_agents");
        let path = d.path().join("openai.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE agent_messages(id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, message_data TEXT NOT NULL, created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP);").unwrap();
        for (scope, item) in [
            (l.scope(), json!({"role":"user","content":"old question"})),
            (
                "other-scope".into(),
                json!({"role":"user","content":"private other session"}),
            ),
            (
                l.scope(),
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"old answer"}]}),
            ),
        ] {
            db.execute(
                "INSERT INTO agent_messages(session_id,message_data) VALUES(?1,?2)",
                rusqlite::params![scope, item.to_string()],
            )
            .unwrap();
        }
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let items = history(&l, d.path()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["content"], "old question");
        assert_eq!(items[1]["content"][0]["text"], "old answer");
        assert_eq!(before, std::fs::read(path).unwrap());
        assert!(!d.path().join("native-history.json").exists());
    }
    #[test]
    fn openai_import_reads_committed_wal_history() {
        let d = tempfile::tempdir().unwrap();
        let l = fixture("openai_agents");
        let db = rusqlite::Connection::open(d.path().join("openai.sqlite")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE agent_messages(id INTEGER PRIMARY KEY,session_id TEXT,message_data TEXT);").unwrap();
        db.execute(
            "INSERT INTO agent_messages VALUES(1,?1,?2)",
            rusqlite::params![
                l.scope(),
                json!({"role":"user","content":"committed WAL"}).to_string()
            ],
        )
        .unwrap();
        assert!(d.path().join("openai.sqlite-wal").exists());
        assert_eq!(
            history(&l, d.path()).unwrap()[0]["content"],
            "committed WAL"
        );
        assert!(!d.path().join("native-history.json").exists());
    }
    #[test]
    fn corrupt_openai_history_is_not_silently_dropped() {
        let d = tempfile::tempdir().unwrap();
        let l = fixture("openai_agents");
        let db = rusqlite::Connection::open(d.path().join("openai.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE agent_messages(id INTEGER PRIMARY KEY,session_id TEXT,message_data TEXT);").unwrap();
        db.execute(
            "INSERT INTO agent_messages VALUES(1,?1,'broken')",
            [l.scope()],
        )
        .unwrap();
        drop(db);
        assert!(history(&l, d.path()).is_err());
    }
    #[test]
    fn opaque_sdk_state_fails_closed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("claude-session.json"), b"{}").unwrap();
        assert!(history(&fixture("claude_agent_sdk"), d.path()).is_err());
    }
    async fn serve(body: &str) -> String {
        let body = format!(
            "{}data: {{\"type\":\"sdk.model_receipt\",\"settled\":true,\"call_id\":\"fixture:0\",\"run_id\":\"fixture\",\"cost_nanos\":1,\"receipt_id\":\"fixture-receipt\"}}\n\n",
            body
        );
        serve_raw(&body).await
    }
    async fn serve_raw(body: &str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = body.to_owned();
        std::thread::spawn(move || {
            use std::io::Read;
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{address}")
    }
    #[tokio::test]
    async fn provider_terminal_without_durable_model_receipt_is_rejected() {
        let url=serve_raw("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n").await;
        let error = sse(
            &reqwest::Client::new(),
            &url,
            "synthetic-run-grant",
            json!({}),
            false,
            &mut String::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "sdk_model_receipt_missing");
    }
    #[tokio::test]
    async fn settled_receipt_cannot_be_rebound_to_another_call() {
        let url=serve("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n").await;
        let error = sse(
            &reqwest::Client::new(),
            &url,
            "synthetic-run-grant",
            json!({"call_id":"other:0"}),
            false,
            &mut String::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "sdk_model_receipt_invalid");
    }
    #[tokio::test]
    async fn openai_wire_terminal() {
        let url=serve("data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n").await;
        let mut text = String::new();
        let r = sse(
            &reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .build()
                .unwrap(),
            &url,
            "fixture-key",
            json!({}),
            false,
            &mut text,
        )
        .await
        .unwrap();
        assert_eq!(text, "hello");
        assert_eq!(r["status"], "completed");
    }
    #[tokio::test]
    async fn incomplete_wire_fails() {
        let url =
            serve("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n")
                .await;
        assert!(
            sse(
                &reqwest::Client::builder()
                    .retry(reqwest::retry::never())
                    .no_proxy()
                    .build()
                    .unwrap(),
                &url,
                "fixture-key",
                json!({}),
                false,
                &mut String::new()
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn refusal_delta_and_terminal_content_are_preserved() {
        let refusal = "I cannot help with that request.";
        let refusal_delta = json!({"type":"response.refusal.delta","delta":refusal});
        let completed = json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":refusal}]}]}});
        let body = format!("data: {refusal_delta}\n\ndata: {completed}\n\n");
        let url = serve(&body).await;
        let mut streamed = String::new();
        let response = sse(
            &reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .build()
                .unwrap(),
            &url,
            "fixture-key",
            json!({}),
            false,
            &mut streamed,
        )
        .await
        .unwrap();
        assert_eq!(streamed, refusal);
        assert_eq!(
            final_text(response["output"].as_array().unwrap(), false),
            refusal
        );
    }
    #[tokio::test]
    async fn anthropic_tool_arguments() {
        let url=serve(concat!(
 "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":2}}}\n\n",
 "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call1\",\"name\":\"utc_time\",\"input\":{}}}\n\n",
 "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
 "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":1}}\n\n",
 "data: {\"type\":\"message_stop\"}\n\n")).await;
        let r = sse(
            &reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .build()
                .unwrap(),
            &url,
            "fixture-key",
            json!({}),
            true,
            &mut String::new(),
        )
        .await
        .unwrap();
        assert_eq!(r["content"][0]["id"], "call1");
        assert_eq!(r["content"][0]["input"], json!({}));
        assert_eq!(r["usage"]["input_tokens"], 2);
        assert_eq!(r["usage"]["output_tokens"], 1);
    }
}

pub(crate) fn ensure_profile(l: &Launch, state: &Path) -> Result<()> {
    let profile = state.join("sdk-profile.json");
    if profile.exists() {
        ensure!(
            serde_json::from_slice::<Value>(&std::fs::read(&profile)?)? == l.profile(),
            "sdk_session_profile_changed"
        );
    } else {
        atomic_json(&profile, &l.profile())?;
    }
    Ok(())
}
