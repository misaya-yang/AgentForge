use super::sdk_migration_parity_lineage::{body, fixture, launch};
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn governed() -> Launch {
    let mut v = body(true);
    v["capability_bridge"]["timeout_seconds"] = json!(1800);
    v["capability_bridge"]["tools"] = json!([{"name":"platform_capability_0","tool":"fixture.write","namespace":"fixture","description":"Fixture","input_schema":{"type":"object"},"id":"fixture","schema_hash":format!("sha256:{}","a".repeat(64)),"effect":"write"}]);
    launch(v)
}
async fn admitted(arguments: Value, terminal: &'static str) {
    let expected = arguments.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (bridge, join) = fixture(2, move |path, request| {
        assert_eq!(path, "/internal/v1/sdk-tools/capabilities/execute");
        assert_eq!(request["arguments"], expected);
        assert!(serde_json::to_vec(&request["arguments"]).unwrap().len() <= 65536);
        assert_eq!(request["namespace"], "fixture");
        let first = observed.fetch_add(1, Ordering::SeqCst) == 0;
        (
            200,
            json!({"run_id":"00000000-0000-4000-8000-000000000001","tool_call_id":"official-call-1","tool":"fixture.write","approval_id":"00000000-0000-4000-8000-000000000002","status":if first {"approval_required"} else {terminal},"expires_at":"2099-01-01T00:00:00Z","preview":{"can_approve":true},"approval_status":if terminal=="succeeded" {"consumed"} else {"rejected"},"execution_id":if first {None} else {Some("fixture-execution")},"response":{"success":terminal=="succeeded"}}),
        )
    });
    let result = bridge
        .invoke(
            &governed(),
            "platform_capability_0",
            arguments,
            "official-call-1",
        )
        .await
        .unwrap();
    assert_eq!(result["status"], terminal);
    assert_eq!(result["response"]["success"], terminal == "succeeded");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    join.join().unwrap();
}
#[tokio::test]
async fn multilingual_action_reaches_approval_and_completion() {
    admitted(json!({"text":"文".repeat(12000)}), "succeeded").await;
}
#[tokio::test]
async fn multilingual_action_rejection_preserves_receipt() {
    admitted(json!({"text":"文".repeat(12000)}), "failed").await;
}
#[tokio::test]
async fn compact_array_avoids_optional_whitespace_overcount() {
    admitted(json!({"values":vec![0;30000]}), "succeeded").await;
}
#[tokio::test]
async fn ascii_exact_65536_byte_limit() {
    let args = json!({"text":"a".repeat(65525)});
    assert_eq!(serde_json::to_vec(&args).unwrap().len(), 65536);
    admitted(args, "succeeded").await;
}
#[tokio::test]
async fn utf8_exact_65536_byte_limit() {
    let args = json!({"text":format!("{}ab","文".repeat(21841))});
    assert_eq!(serde_json::to_vec(&args).unwrap().len(), 65536);
    admitted(args, "succeeded").await;
}
#[tokio::test]
async fn oversized_arguments_are_fenced_before_dispatch() {
    let bridge = Bridge {
        client: reqwest::Client::new(),
        origin: None,
    };
    for args in [
        json!({"text":"a".repeat(65526)}),
        json!({"text":"文".repeat(21842)}),
    ] {
        assert_eq!(
            bridge
                .invoke(
                    &governed(),
                    "platform_capability_0",
                    args,
                    "official-call-1"
                )
                .await
                .unwrap_err()
                .to_string(),
            "sdk_capability_arguments_invalid"
        );
    }
}
#[test]
fn malformed_provider_arguments_cannot_enter_json_value_boundary() {
    for raw in [
        r#"{"value":NaN}"#,
        r#"{"value":1e999}"#,
        r#"{"value":"\ud800"}"#,
    ] {
        assert!(serde_json::from_str::<Value>(raw).is_err());
    }
}
