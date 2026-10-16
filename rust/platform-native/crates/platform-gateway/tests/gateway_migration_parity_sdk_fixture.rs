//! Preserve the original cross-language fixture/source contract without a Python test runner.
use serde_json::{Value, json};
fn read(path: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .join(path),
    )
    .unwrap()
}
#[test]
fn shared_sdk_sse_fixture_defines_inner_envelope_and_terminals() {
    let fixture: Value =
        serde_json::from_str(&read("sdk/fixtures/sse_inner_envelopes.json")).unwrap();
    assert_eq!(
        fixture["text_delta"]["sse"],
        "data: {\"event_type\":\"text_delta\",\"data\":\"Hi\"}\n\n"
    );
    assert_eq!(fixture["text_delta"]["text"], "Hi");
    for name in ["done", "error", "cancelled", "run_finished", "run_error"] {
        assert_eq!(fixture[name]["event_type"], name);
    }
    for (name, value) in [
        ("null_data", Value::Null),
        ("number_data", json!(42)),
        ("boolean_data", json!(true)),
        ("array_data", json!([1, 2])),
    ] {
        assert_eq!(fixture[name]["value"], value);
    }
}
#[test]
fn java_and_dart_native_contracts_consume_shared_fixture() {
    for (path, needles) in [
        (
            "sdk/java/src/test/java/com/aigateway/ai/SSEParserTest.java",
            vec!["sse_inner_envelopes.json"],
        ),
        (
            "sdk/java/src/main/java/com/aigateway/ai/SSEParser.java",
            vec!["payload.remove(\"data\")", "event.isTerminal()"],
        ),
        (
            "sdk/java/src/main/java/com/aigateway/ai/ChatClient.java",
            vec![
                "EventType.ERROR, EventType.RUN_ERROR",
                "if (!unsuccessfulTerminal[0])",
            ],
        ),
        (
            "sdk/dart/ai_gateway_sdk/test/streaming_test.dart",
            vec!["sse_inner_envelopes.json"],
        ),
        (
            "sdk/dart/ai_gateway_sdk/lib/src/streaming.dart",
            vec!["payload.remove('data')", "event.isTerminal"],
        ),
    ] {
        let source = read(path);
        for needle in needles {
            assert!(source.contains(needle), "{path}: {needle}");
        }
    }
}
