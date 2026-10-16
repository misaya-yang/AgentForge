//! Exact native-source admission assertions formerly hosted by test_native_approval_receipt_readback.py.
//! These remain source contracts, not PostgreSQL/HTTP execution evidence.
use std::path::Path;
fn read(relative: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join(relative),
    )
    .unwrap()
}
fn gateway(name: &str) -> String {
    read(&format!(
        "rust/platform-native/crates/platform-gateway/src/migration_agent/{name}"
    ))
}
fn runtime(name: &str) -> String {
    read(&format!(
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/{name}"
    ))
}
fn before(s: &str, a: &str, b: &str) {
    assert!(
        s.find(a).unwrap() < s.find(b).unwrap(),
        "{a} must precede {b}"
    );
}
#[test]
fn both_public_routes_retain_auth_before_receipt_readback() {
    let source = gateway("routes.rs");
    for marker in [
        "(\"POST\",[\"approvals\",approval_id,\"decision\"])",
        "(\"POST\",[\"approvals\",id])",
    ] {
        let route = source.split_once(marker).unwrap().1.lines().next().unwrap();
        before(
            route,
            "sources.require_conversation",
            "read_approval_decision",
        );
        assert!(route.contains("if !receipt_only{let preview=approval::owner_approval_preview"));
        assert!(route.contains("APPROVAL_ACTION_UNVERIFIED"));
    }
    assert!(source.contains("read_approval_decision(&scope,&thread.runtime_thread_id,approval_id"));
    assert!(source.contains("read_approval_decision(&scope,text(&row,\"harness_thread_id\"),id"));
    let v1 = source
        .split_once("(\"POST\",[\"approvals\",id])")
        .unwrap()
        .1
        .lines()
        .next()
        .unwrap();
    before(
        v1,
        "sdk_approvals::decide",
        "bounded(&payload,\"reason\",0,usize::MAX,false)?",
    );
    before(
        v1,
        "bounded(&payload,\"reason\",0,usize::MAX,false)?",
        "read_approval_decision",
    );
}
#[test]
fn missing_receipt_cannot_fall_through_to_mutation() {
    let source = runtime("postgres_store/approval_decision.rs");
    before(
        &source,
        "validate_receipt_thread(receipt_only_thread, root_id)?",
        "let saved = sqlx::query(",
    );
    before(&source, "validate_required(", "let saved = sqlx::query(");
    let resolve = regex::Regex::new(r"if let Some\(event\) =\s*resolve_decision_receipt\(")
        .unwrap()
        .find(&source)
        .unwrap()
        .start();
    assert!(source.find("let saved = sqlx::query(").unwrap() < resolve);
    assert!(resolve < source.find("UPDATE assistant_tool_approvals").unwrap());
    for marker in [
        "None if receipt_only => Err(conflict(\"approval decision receipt missing\"))",
        "!same_decision(&saved.event, proposed)",
        "saved_id != event_id",
    ] {
        assert!(source.contains(marker), "{marker}");
    }
}
#[test]
fn receipt_mode_cannot_reach_native_resolution_or_wakeup() {
    let source = runtime("approval_control.rs");
    before(
        &source,
        "if receipt_only_thread.is_some()",
        "resolve_server_request_async(",
    );
    let block = source
        .split_once("if receipt_only_thread.is_some()")
        .unwrap()
        .1
        .split_once("// Legacy native requests still require")
        .unwrap()
        .0;
    for marker in [
        "receipt_only_thread,",
        "if saved.changed {",
        "return Ok(ApprovalProjection",
    ] {
        assert!(block.contains(marker));
    }
    let http = runtime("http_service/approvals.rs");
    assert!(http.contains("if body.receipt_only.is_none() {\n        let _ = state.events.send"));
    before(&http, "authorize(&headers", ".decide(");
}
#[test]
fn normal_wire_body_is_unchanged_and_readback_has_no_retry_fallback() {
    let source = gateway("control.rs");
    let block = source
        .split_once("async fn submit_approval_decision(")
        .unwrap()
        .1
        .split_once("pub async fn complete_run")
        .unwrap()
        .0;
    assert!(block.contains("if let Some(thread) = receipt_only_thread"));
    let compact: String = block.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        compact.contains("payload[\"receipt_only\"]=json!({\"thread_id\":super::uuid(thread)?})")
    );
    assert_eq!(block.matches(".send()").count(), 1);
    assert!(!block.contains("decide_approval("));
}
