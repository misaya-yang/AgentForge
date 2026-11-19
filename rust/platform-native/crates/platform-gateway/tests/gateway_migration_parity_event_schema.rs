//! Source tests/contract/test_event_schema_evolution.py against reusable production event contracts.
use ai_platform_gateway_foundation::event_envelope::{
    EventEnvelope, PAYLOAD_MODELS, PayloadSchema, STREAM_NAMES, UsageRecordedV1, get_stream,
    payload_for,
};
use serde_json::{Value, json};
fn baseline() -> Value {
    json!({"event_id":"12345678-1234-4234-8234-123456789abc","event_type":"usage.recorded.v1","schema_version":1,"occurred_at":"2026-10-15T00:00:00Z","producer":"gateway","tenant_id":"tenant-a","request_id":"request-a","payload":{"tenant_id":"tenant-a","user_id":"user-a","model":"model-a","timestamp":1.0}})
}
#[test]
fn event_envelope_newer_context_fields_are_optional() {
    let minimal: EventEnvelope<UsageRecordedV1> = serde_json::from_value(baseline()).unwrap();
    assert!(minimal.traceparent.is_none());
    let serialized = serde_json::to_value(&minimal).unwrap();
    // Source's newer context field is optional; baseline envelope remains consumable.
    assert!(serialized.get("traceparent").is_none_or(Value::is_null));
    let mut future = baseline();
    future["future_optional_context"] = json!({"value":"ignored-by-old-consumer"});
    let envelope: EventEnvelope<UsageRecordedV1> = serde_json::from_value(future).unwrap();
    assert_eq!(envelope.request_id, "request-a");
}
#[test]
fn registered_event_payloads_have_stream_routes() {
    assert!(!PAYLOAD_MODELS.is_empty());
    for (event_type, schema) in PAYLOAD_MODELS {
        assert!(STREAM_NAMES.iter().any(|(name, _)| name == event_type));
        assert_eq!(payload_for(event_type).unwrap(), *schema);
        assert!(!get_stream(event_type).unwrap().is_empty());
    }
}
#[test]
fn event_schema_versions_match_event_type_suffix() {
    for (event_type, schema) in PAYLOAD_MODELS {
        let suffix = event_type.rsplit_once(".v").unwrap().1;
        assert!(!suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
        match schema {
            PayloadSchema::UsageRecordedV1 => {
                assert_eq!(*event_type, UsageRecordedV1::EVENT_TYPE);
                let mut value = baseline();
                value.as_object_mut().unwrap().remove("schema_version");
                let envelope: EventEnvelope<UsageRecordedV1> =
                    serde_json::from_value(value).unwrap();
                assert_eq!(envelope.schema_version, suffix.parse::<i64>().unwrap());
            }
        }
    }
}
