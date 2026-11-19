//! tests/services/assistant/test_redaction.py: merged trace/log pattern contract.
use ai_platform_gateway::migration_compat::eval_observation::redact_trace_text;
#[test]
fn merged_source_patterns_remove_secrets_and_keep_safe_markers() {
    for (input, absent, present) in [
        (
            "Authorization: Bearer super-secret-value",
            vec!["super-secret-value"],
            vec!["Authorization: Bearer [redacted]"],
        ),
        (
            "curl -H 'Bearer raw-token-123'",
            vec!["raw-token-123"],
            vec!["Bearer [redacted]"],
        ),
        (
            "api_key=abc123XYZ and password: hunter2",
            vec!["abc123XYZ", "hunter2"],
            vec!["api_key=[redacted]", "password=[redacted]"],
        ),
        (
            r#"{"api_key": "sk-abc123XYZsecret"}"#,
            vec!["sk-abc123XYZsecret"],
            vec!["[redacted]"],
        ),
        (
            "postgres://user:pw@host:5432/db",
            vec!["user:pw@host"],
            vec!["postgres://[redacted]"],
        ),
        (
            "Incorrect API key provided: sk-fb4d4***********************f34c",
            vec!["sk-fb4d4", "f34c"],
            vec!["sk-[redacted]"],
        ),
        (
            "google key AIzaSyA123456789012345678901234567890",
            vec!["AIzaSyA123456789012345678901234567890"],
            vec!["AIza[redacted]"],
        ),
        (
            "https://provider.test/generate?key=AQ.vertex-secret-123&alt=sse",
            vec!["AQ.vertex-secret-123"],
            vec!["key=[redacted]", "alt=sse"],
        ),
        (
            "vertex credential AQ.vertex-secret-456",
            vec!["AQ.vertex-secret-456"],
            vec!["AQ.[redacted]"],
        ),
    ] {
        let result = redact_trace_text(input, None);
        for secret in absent {
            assert!(!result.contains(secret), "secret retained for {input}");
        }
        for marker in present {
            assert!(result.contains(marker), "missing {marker}: {result}");
        }
    }
}
#[test]
fn truncation_is_explicit_and_empty_text_is_preserved() {
    let input = "x".repeat(1000);
    assert_eq!(redact_trace_text(&input, None), input);
    assert_eq!(
        redact_trace_text(&input, Some(10)),
        "xxxxxxxxxx...[truncated]"
    );
    assert_eq!(redact_trace_text("", None), "");
    // Rust accepts a string rather than an arbitrary exception/None object.
    // Preserve the observable safe exception description after typed formatting.
    assert_eq!(
        redact_trace_text("_BoomError: db unreachable", None),
        "_BoomError: db unreachable"
    );
}
