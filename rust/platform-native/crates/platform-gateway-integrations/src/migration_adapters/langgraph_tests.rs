use super::*;
use futures_util::FutureExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

const FIXTURES: &str = include_str!("../../../../../../tests/fixtures/adapters/langgraph_stream_terminal.json");

struct DropSignal(Arc<AtomicUsize>);
impl Drop for DropSignal {
    fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
}

async fn fixture(parts: Vec<Vec<u8>>, stall: bool) -> (LangGraphStream, Arc<Semaphore>, Arc<AtomicUsize>) {
    let slots = Arc::new(Semaphore::new(1));
    let permit = slots.clone().acquire_owned().await.unwrap();
    let dropped = Arc::new(AtomicUsize::new(0));
    let guard = DropSignal(dropped.clone());
    let mut parts: VecDeque<_> = parts.into();
    let body = reqwest::Body::wrap_stream(futures_util::stream::poll_fn(move |_| {
        let _keep = &guard;
        match parts.pop_front() {
            Some(bytes) => std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(bytes))),
            None if stall => std::task::Poll::Pending,
            None => std::task::Poll::Ready(None),
        }
    }));
    let response = axum::http::Response::builder().status(200).body(body).unwrap();
    let stream = LangGraphStream {
        lines: LangGraphLines::new(HttpResponse { response: response.into(), permit: Some(permit) }, "/runs/stream".into()),
        request_id: "request".into(), index: 0, event: String::new(), usage: None,
        thinking: false, finished: false, pending: VecDeque::new(), projection: Projection::default(),
    };
    (stream, slots, dropped)
}

#[tokio::test]
async fn langgraph_terminal_error_wire_cases_use_the_production_stream() {
    let cases: Vec<Value> = serde_json::from_str(FIXTURES).unwrap();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let wire = case["wire"].as_str().unwrap().as_bytes();
        // Every split point checks the actual incremental parser, not a copy.
        for split in 0..=wire.len() {
            let parts = vec![wire[..split].to_vec(), wire[split..].to_vec()];
            let (mut stream, slots, dropped) = fixture(parts, false).await;
            let mut text = String::new();
            let mut finals = Vec::new();
            let mut errors = 0;
            let mut indexes = Vec::new();
            loop {
                match stream.next().await {
                    Ok(Some(chunk)) => {
                        indexes.push(chunk.chunk_index);
                        assert!(!format!("{chunk:?}").contains("SYNTHETIC_PRIVATE_PROVIDER_DETAIL"));
                        if chunk.event_type == StreamEventType::TextDelta {
                            text.push_str(data_value(&chunk.content).as_str().unwrap());
                        }
                        if chunk.is_final { finals.push(chunk); }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        assert!(matches!(error, DispatchError::AdapterValidationFailed("langgraph_provider_error")), "{name}/{split}: {error:?}");
                        errors += 1;
                    }
                }
                assert!(indexes.len() + errors < 10, "{name}/{split}: terminal loop");
            }
            assert_eq!(text, case["text"].as_str().unwrap(), "{name}/{split}");
            assert_eq!(indexes, (0..indexes.len() as i64).collect::<Vec<_>>());
            if case["error"] == true {
                assert_eq!(errors, 1, "{name}/{split}: upstream failure must be visible");
                assert!(finals.is_empty(), "{name}/{split}: no fabricated success/usage");
                assert!(stream.usage.is_none());
            } else {
                assert_eq!(errors, 0, "{name}/{split}");
                assert_eq!(finals.len(), 1, "{name}/{split}");
                let usage = finals[0].metadata.as_ref().and_then(|m| m.get("usage"));
                assert_eq!(usage, case.get("usage"), "{name}/{split}");
            }
            assert!(stream.finished && stream.pending.is_empty());
            assert!(stream.lines.response.is_none() && stream.lines.buffer.is_empty());
            assert_eq!(slots.available_permits(), 1, "{name}/{split}");
            assert_eq!(dropped.load(Ordering::SeqCst), 1, "{name}/{split}");
            assert!(stream.next().await.unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn langgraph_error_header_fails_without_waiting_for_private_body_or_eof() {
    let (mut stream, slots, dropped) = fixture(vec![b"event: error|worker:fixture\n".to_vec()], true).await;
    let result = stream.next().now_or_never().expect("must not wait for an error body");
    assert!(matches!(result, Err(DispatchError::AdapterValidationFailed("langgraph_provider_error"))));
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert!(stream.next().await.unwrap().is_none());
}

#[tokio::test]
async fn langgraph_close_and_drop_release_a_partial_error_frame_without_success() {
    for explicit in [false, true] {
        let (mut stream, slots, dropped) = fixture(vec![b"event: err".to_vec()], true).await;
        assert!(stream.next().now_or_never().is_none());
        assert_eq!(slots.available_permits(), 0);
        if explicit { Box::new(stream).close().await.unwrap(); } else { drop(stream); }
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn langchain_tool_outcomes_survive_stream_and_session_projection() {
    use ai_platform_gateway_foundation::{dispatcher::StreamStatus, stream_statistics::StreamStatistics};
    let fixture_cases: Value = serde_json::from_str(include_str!("../../../../../../tests/fixtures/adapters/langchain_tool_roundtrip.json")).unwrap();
    for case in fixture_cases["cases"].as_array().unwrap() {
        let messages = case["messages"].as_array().unwrap();
        let result_message = messages.iter().find(|m| m["type"] == "tool").unwrap();
        let expected_status = if case["status"] == "error" { "error" } else { "completed" };
        for mode in ["messages", "updates"] {
            let mut wire = String::new();
            for message in messages {
                let (event, data) = if mode == "updates" && message["type"] == "tool" {
                    ("updates", json!({"tools":{"messages":[message]}}))
                } else {
                    ("messages", json!([message, {}]))
                };
                wire.push_str(&format!("event: {event}\ndata: {data}\n\n"));
            }
            wire.push_str("event: end\ndata: {}\n\n");
            let (mut stream, _, _) = fixture(vec![wire.into_bytes()], false).await;
            let mut stats = StreamStatistics::default();
            let mut tool_results = 0;
            while let Some(chunk) = stream.read().await.unwrap() {
                if chunk.event_type == StreamEventType::ToolResult {
                    tool_results += 1;
                    assert_eq!(chunk.tool_call.as_ref().unwrap().status, expected_status, "{} {mode}", case["name"]);
                    assert_eq!(serde_json::to_value(&chunk.content.data).unwrap(), result_message["content"]);
                }
                stats.observe(chunk, 1);
            }
            assert_eq!(tool_results, 1);
            let finalized = stats.finalize(StreamStatus::Success, "Question", 2, Some(3)).unwrap();
            let metadata = finalized.session_metadata.unwrap();
            assert_eq!(metadata["tool_calls"][0]["status"], expected_status);
            assert_eq!(metadata["tool_calls"][0]["result"], result_message["content"]);
            if messages.last().unwrap()["type"] == "tool" {
                assert_eq!(finalized.output_text, "");
            }
        }
        let local = extract_local_event(&json!({"event":"on_tool_end","run_id":"run","name":"lookup","data":{"output":result_message}}), &mut HashSet::new()).unwrap();
        assert_eq!(local.tool_call.unwrap().status, expected_status);
    }
}
