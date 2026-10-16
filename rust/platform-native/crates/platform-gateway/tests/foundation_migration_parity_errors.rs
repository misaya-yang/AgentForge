//! Source tests/core/test_errors.py observable code/category/status/wire contracts.
use ai_platform_gateway::migration_host::errors::{DEFINITIONS, GatewayException};
use serde_json::json;
fn error(name: &str) -> GatewayException {
    let code = DEFINITIONS.iter().find(|d| d.name == name).unwrap().code;
    GatewayException::new(code, None, None, None, None)
}
#[test]
fn error_categories_status_codes_and_retry_policy_match_canonical_definitions() {
    for (name, category) in [
        ("INVALID_REQUEST", 1),
        ("INTERNAL_ERROR", 2),
        ("SESSION_ERROR", 3),
        ("ADAPTER_ERROR", 4),
    ] {
        assert_eq!(error(name).category(), category, "{name}");
    }
    for (name, status) in [
        ("AUTHENTICATION_REQUIRED", 401),
        ("RATE_LIMIT_EXCEEDED", 429),
        ("SERVICE_NOT_FOUND", 404),
    ] {
        assert_eq!(error(name).status(), status, "{name}");
    }
    assert!(error("TIMEOUT").retryable);
    assert!(!error("PERMISSION_DENIED").retryable);
}
#[test]
fn gateway_exception_preserves_explicit_message_details_and_canonical_wire_code() {
    let code = error("SERVICE_NOT_FOUND").code;
    let exception = GatewayException::new(
        code,
        Some("Service xyz not found".into()),
        Some(json!({"service_id":"xyz"})),
        None,
        None,
    );
    assert_eq!(exception.code, code);
    assert_eq!(exception.message, "Service xyz not found");
    assert_eq!(exception.status(), 404);
    assert_eq!(exception.details.as_ref().unwrap()["service_id"], "xyz");
    let wire = exception.payload(false);
    assert_eq!(wire["error"]["code"], code);
    assert_eq!(wire["error"]["message"], "Service xyz not found");
    assert_eq!(wire["error"]["details"]["service_id"], "xyz");
}
