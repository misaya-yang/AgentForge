//! Pure trace-capture contracts from source helper suite; no persistence simulation.
use ai_platform_gateway::{
    migration_compat::{common::CompatUser, proxy_trace::build_langgraph_trace_payload},
    migration_knowledge_query::rag_trace::{self, RagTraceInput},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
fn start() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}
fn langgraph(path: &str, streaming: bool, traceparent: Option<&str>) -> Value {
    build_langgraph_trace_payload(
        &CompatUser {
            user_id: "user-a".into(),
            tenant_id: "tenant-a".into(),
            request_id: "req-1".into(),
            ..Default::default()
        },
        "POST",
        path,
        start(),
        start() + Duration::milliseconds(250),
        "succeeded",
        Some(200),
        None,
        traceparent,
        streaming,
    )
}
#[test]
fn retrieval_paths_cover_knowledge_and_qa() {
    for path in [
        "dataset-1/retrieve",
        "knowledge/dataset-1/retrieve",
        "dataset-1/hit_test",
        "dataset-1/qa",
        "dataset-1/qa/stream",
    ] {
        assert!(rag_trace::is_retrieve_path(path));
    }
    assert!(!rag_trace::is_retrieve_path("dataset-1/documents"));
}
#[test]
fn langgraph_trace_extracts_ids_and_canonical_lifecycle() {
    let p = langgraph(
        "/threads/thread-1/runs/run-1/stream",
        true,
        Some("00-abc123def456789012345678901234-0123456789abcdef-01"),
    );
    assert_eq!(p["trace_family"], "langgraph_proxy");
    assert_eq!(p["thread_id"], "thread-1");
    assert_eq!(p["run_id"], "run-1");
    assert_eq!(p["otel_trace_id"], "abc123def456789012345678901234");
    assert_eq!(p["spans"][0]["span_kind"], "lifecycle");
    assert_eq!(p["events"][0]["event_type"], "proxy_request_accepted");
}
#[test]
fn langgraph_route_matrix_and_parent_linkage() {
    for (path, thread, run, assistant, streaming) in [
        (
            "/assistants/assistant-1/runs/stream",
            None,
            None,
            Some("assistant-1"),
            true,
        ),
        (
            "/threads/thread-1/runs/stream",
            Some("thread-1"),
            None,
            None,
            true,
        ),
        (
            "/threads/thread-1/runs/run-1/stream",
            Some("thread-1"),
            Some("run-1"),
            None,
            true,
        ),
        (
            "/threads/thread-1/runs/run-1/wait",
            Some("thread-1"),
            Some("run-1"),
            None,
            false,
        ),
        (
            "/threads/thread-1/runs/run-1/cancel",
            Some("thread-1"),
            Some("run-1"),
            None,
            false,
        ),
    ] {
        let p = langgraph(path, streaming, None);
        assert_eq!(p["trace_family"], "langgraph_proxy");
        assert_eq!(p["thread_id"], json!(thread));
        assert_eq!(p["session_id"], json!(thread));
        assert_eq!(p["run_id"], json!(run));
        assert_eq!(p["metadata"]["assistant_id"], json!(assistant));
        assert_eq!(p["metrics"]["streaming"], streaming);
        assert!(p["spans"][0]["parent_span_id"].is_null());
        assert_eq!(p["spans"][1]["parent_span_id"], p["spans"][0]["span_id"]);
        assert_eq!(p["spans"][1]["span_kind"], "gateway_proxy");
    }
}
fn rag(dataset: &str, documents: Vec<Value>, metadata: Value, trace_id: Option<&str>) -> Value {
    rag_trace::build_rag_trace(RagTraceInput {
        request_id: "req-2",
        tenant_id: "tenant-a",
        user_id: "user-a",
        dataset_id: Some(dataset),
        query: "refund policy",
        started: start(),
        ended: start() + Duration::milliseconds(100),
        status: "succeeded",
        upstream_status: Some(200),
        document_count: 2,
        error: None,
        traceparent: None,
        documents,
        metadata,
        trace_id,
    })
}
#[test]
fn rag_trace_preserves_dataset_lifecycle_and_backend_identity() {
    let p = rag("dataset-9", vec![], json!({}), None);
    assert_eq!(p["trace_family"], "rag");
    assert_eq!(p["metadata"]["dataset_id"], "dataset-9");
    assert_eq!(p["metadata"]["answer_source"], "retrieval_only");
    assert_eq!(p["spans"][1]["span_kind"], "retriever");
    assert_eq!(p["spans"][1]["parent_span_id"], p["spans"][0]["span_id"]);
    assert_eq!(p["events"][1]["event_type"], "rag_retrieval_completed");
    for path in ["dataset-1/retrieve", "/api/v1/knowledge/dataset-1/retrieve"] {
        let dataset = rag_trace::dataset_id(path).unwrap();
        let docs = rag_trace::retrieval_documents(&[json!({"text":"hello chunk"})], 8, 1500);
        let p = rag(
            &dataset,
            docs,
            json!({}),
            Some("d04d53c8-acde-49d0-b3eb-49890dbd5673"),
        );
        assert_eq!(p["metadata"]["dataset_id"], "dataset-1");
        assert_eq!(p["trace_id"], "d04d53c8-acde-49d0-b3eb-49890dbd5673");
        assert!(
            !p["spans"][1]["attributes"]["retrieval"]["documents"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
#[test]
fn retrieve_parsers_keep_evaluation_content_and_document_count() {
    assert_eq!(rag_trace::parse_request(br#"{"query":"hello"}"#), "hello");
    assert_eq!(
        rag_trace::parse_response(br#"{"results":[{"id":1},{"id":2}]}"#)
            .0
            .len(),
        2
    );
    let(results,metadata)=rag_trace::parse_response(br#"{"results":[{"text":"Refund within 30 days","score":0.9},{"content":"Support email"}]}"#);
    assert_eq!(results.len(), 2);
    assert!(metadata.is_object());
    let docs = rag_trace::retrieval_documents(&results, 8, 1500);
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0]["content_eval"], "Refund within 30 days");
    assert!(docs[0].get("content_preview").is_some());
}
#[test]
fn qa_sse_done_preserves_backend_trace() {
    let body=b"data: {\"event\":\"done\",\"data\":{\"result\":{\"context_segments\":[{\"text\":\"answer context\"}],\"retrieval_metadata\":{},\"trace_id\":\"d04d53c8-acde-49d0-b3eb-49890dbd5673\",\"query_fingerprint\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}}\n\n";
    let (results, metadata) = rag_trace::parse_response(body);
    assert_eq!(results[0]["text"], "answer context");
    assert_eq!(metadata["trace_id"], "d04d53c8-acde-49d0-b3eb-49890dbd5673");
}
