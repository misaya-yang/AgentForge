//! Generic first-party event envelope and independent schema/stream registries.
//! Unknown optional producer fields are ignored, matching the source contracts.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

fn version_one() -> i64 {
    1
}
fn success() -> String {
    "success".into()
}
fn chat() -> String {
    "chat".into()
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope<T = UsageRecordedV1> {
    #[serde(default = "Uuid::new_v4")]
    pub event_id: Uuid,
    pub event_type: String,
    #[serde(default = "version_one", deserialize_with = "integer")]
    pub schema_version: i64,
    #[serde(default = "Utc::now", deserialize_with = "datetime")]
    pub occurred_at: DateTime<Utc>,
    pub producer: String,
    pub tenant_id: String,
    pub request_id: String,
    #[serde(default)]
    pub traceparent: Option<String>,
    pub payload: T,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UsageRecordedV1 {
    pub tenant_id: String,
    pub user_id: String,
    pub model: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub service_id: String,
    #[serde(default)]
    pub assistant_id: String,
    #[serde(default, deserialize_with = "integer")]
    pub input_tokens: i64,
    #[serde(default, deserialize_with = "integer")]
    pub output_tokens: i64,
    #[serde(default, deserialize_with = "integer")]
    pub latency_ms: i64,
    #[serde(default, deserialize_with = "integer")]
    pub first_token_ms: i64,
    #[serde(default, deserialize_with = "integer")]
    pub request_total_duration_ms: i64,
    #[serde(default)]
    pub error_type: Option<String>,
    #[serde(default = "success")]
    pub status: String,
    #[serde(default = "chat")]
    pub request_type: String,
    #[serde(deserialize_with = "number")]
    pub timestamp: f64,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}
impl UsageRecordedV1 {
    pub const EVENT_TYPE: &'static str = "usage.recorded.v1";
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadSchema {
    UsageRecordedV1,
}
pub const PAYLOAD_MODELS: &[(&str, PayloadSchema)] =
    &[(UsageRecordedV1::EVENT_TYPE, PayloadSchema::UsageRecordedV1)];
pub const STREAM_NAMES: &[(&str, &str)] =
    &[(UsageRecordedV1::EVENT_TYPE, "events:usage:recorded:v1")];
pub const DLQ_SUFFIX: &str = ":dlq";
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventDeserializationError {
    MalformedEnvelope,
    PayloadValidation,
    UnknownPayload,
    UnknownStream,
}
impl std::fmt::Display for EventDeserializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MalformedEnvelope => "malformed event envelope",
            Self::PayloadValidation => "invalid registered event payload",
            Self::UnknownPayload => "no payload schema registered for event type",
            Self::UnknownStream => "unknown event stream type",
        })
    }
}
impl std::error::Error for EventDeserializationError {}
pub fn payload_for(event_type: &str) -> Result<PayloadSchema, EventDeserializationError> {
    PAYLOAD_MODELS
        .iter()
        .find(|(name, _)| *name == event_type)
        .map(|(_, schema)| *schema)
        .ok_or(EventDeserializationError::UnknownPayload)
}
pub fn get_stream(event_type: &str) -> Result<&'static str, EventDeserializationError> {
    STREAM_NAMES
        .iter()
        .find(|(name, _)| *name == event_type)
        .map(|(_, stream)| *stream)
        .ok_or(EventDeserializationError::UnknownStream)
}
pub fn dlq_for(stream: &str) -> String {
    format!("{stream}{DLQ_SUFFIX}")
}
pub fn parse_envelope(
    raw_json: &[u8],
) -> Result<EventEnvelope<UsageRecordedV1>, EventDeserializationError> {
    let loose: EventEnvelope<Value> = serde_json::from_slice(raw_json)
        .map_err(|_| EventDeserializationError::MalformedEnvelope)?;
    if !loose.payload.is_object() {
        return Err(EventDeserializationError::MalformedEnvelope);
    }
    match payload_for(&loose.event_type)? {
        PayloadSchema::UsageRecordedV1 => serde_json::from_slice(raw_json)
            .map_err(|_| EventDeserializationError::PayloadValidation),
    }
}
fn number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = Value::deserialize(d)?;
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
        _ => None,
    };
    n.filter(|n| n.is_finite())
        .ok_or_else(|| serde::de::Error::custom("invalid event number"))
}
fn integer<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let v = Value::deserialize(d)?;
    if let Some(n) = v.as_i64() {
        return Ok(n);
    }
    if let Value::String(s) = &v {
        if let Ok(n) = s.trim().parse::<i64>() {
            return Ok(n);
        }
    }
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
        _ => None,
    };
    match n {
        Some(n)
            if n.is_finite()
                && n.fract() == 0.0
                && n >= i64::MIN as f64
                && n < -(i64::MIN as f64) =>
        {
            Ok(n as i64)
        }
        _ => Err(serde::de::Error::custom("invalid event integer")),
    }
}

fn datetime<'de, D: serde::Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
    let value = Value::deserialize(d)?;
    if let Some(s) = value.as_str() {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Ok(dt.with_timezone(&Utc));
        }
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
            return Ok(dt.and_utc());
        }
    }
    let n = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
    if let Some(mut n) = n.filter(|n| n.is_finite()) {
        if n.abs() > 20_000_000_000.0 {
            n /= 1000.0;
        }
        let whole = n.floor();
        if whole >= i64::MIN as f64 && whole < -(i64::MIN as f64) {
            if let Some(dt) =
                DateTime::from_timestamp(whole as i64, ((n - whole) * 1_000_000_000.0) as u32)
            {
                return Ok(dt);
            }
        }
    }
    Err(serde::de::Error::custom("invalid event datetime"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn minimal() -> Value {
        json!({"event_type":"usage.recorded.v1","producer":"gateway","tenant_id":"tenant-a","request_id":"request-a","payload":{"tenant_id":"tenant-a","user_id":"user-a","model":"test-model","timestamp":1.5}})
    }
    #[test]
    fn typed_payload_defaults_and_unknown_optional_fields_are_source_compatible() {
        let mut value = minimal();
        value["future_optional"] = json!("ignored");
        value["payload"]["future_optional"] = json!({"ignored":true});
        let parsed = parse_envelope(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.event_id.get_version_num(), 4);
        assert!(parsed.traceparent.is_none());
        assert_eq!(parsed.payload.provider, "");
        assert_eq!(parsed.payload.service_id, "");
        assert_eq!(parsed.payload.assistant_id, "");
        assert_eq!(parsed.payload.input_tokens, 0);
        assert_eq!(parsed.payload.output_tokens, 0);
        assert_eq!(parsed.payload.latency_ms, 0);
        assert_eq!(parsed.payload.first_token_ms, 0);
        assert_eq!(parsed.payload.request_total_duration_ms, 0);
        assert!(parsed.payload.error_type.is_none());
        assert_eq!(parsed.payload.status, "success");
        assert_eq!(parsed.payload.request_type, "chat");
        assert!(parsed.payload.metadata.is_empty());
        let encoded = serde_json::to_value(parsed).unwrap();
        assert!(encoded.get("future_optional").is_none());
        assert!(encoded["payload"].get("future_optional").is_none());
    }
    #[test]
    fn missing_or_malformed_typed_payload_is_distinct_from_poison_envelope() {
        for key in ["tenant_id", "user_id", "model", "timestamp"] {
            let mut value = minimal();
            value["payload"].as_object_mut().unwrap().remove(key);
            assert_eq!(
                parse_envelope(&serde_json::to_vec(&value).unwrap()),
                Err(EventDeserializationError::PayloadValidation)
            );
        }
        let mut value = minimal();
        value.as_object_mut().unwrap().remove("payload");
        assert_eq!(
            parse_envelope(&serde_json::to_vec(&value).unwrap()),
            Err(EventDeserializationError::MalformedEnvelope)
        );
        let mut value = minimal();
        value["payload"] = json!([]);
        assert_eq!(
            parse_envelope(&serde_json::to_vec(&value).unwrap()),
            Err(EventDeserializationError::MalformedEnvelope)
        );
        let mut value = minimal();
        value["payload"]["input_tokens"] = json!(1.5);
        assert_eq!(
            parse_envelope(&serde_json::to_vec(&value).unwrap()),
            Err(EventDeserializationError::PayloadValidation)
        );
    }
}
