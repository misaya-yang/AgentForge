//! Byte-preserving proof verification contract used by Knowledge capability ingress.
pub use ai_platform_capability_contract::broker_proof as broker_proof_v3;
use super::{ContentError, python_json, sha256};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use serde_json::Value;
fn string<'a>(v: &'a Value, key: &str) -> &'a str { v.get(key).and_then(Value::as_str).unwrap_or("") }
fn hex_id(value: &str, prefix: &str, len: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|s| s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, ContentError> {
    Ok(super::python_json(value, false, false).into_bytes())
}
pub fn constant_time_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |n, (a, b)| n | (a ^ b)) == 0
}
#[derive(Clone, Debug)]
pub struct CapabilityScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub arguments_hash: String,
}
pub fn verify_broker_proof_for_window(secret:&[u8],header:&str,method:&str,path:&str,body:&Value,scope:&CapabilityScope,now:i64,deadline:Option<u64>)->Result<Value,ContentError>{
    if header.starts_with("v1.") && broker_proof_v3::deadline_admitted(u64::try_from(now).unwrap_or(0).saturating_mul(1000),deadline,broker_proof_v3::verify_only_required()) {
        return verify_capability_proof(secret,header,method,path,body,scope,now);
    }
    verify_broker_proof(secret,header,method,path,body,scope,now)
}
pub fn verify_broker_proof(secret: &[u8], header: &str, method: &str, path: &str, body: &Value, scope: &CapabilityScope, now: i64) -> Result<Value, ContentError> {
    if broker_proof_v3::verify_only_required() && !secret.is_empty() { return Err(ContentError::new(503,"consumer broker signing secret forbidden")); }
    let now_ms = u64::try_from(now).unwrap_or(0).saturating_mul(1000);
    if header.starts_with(broker_proof_v3::wire::ENVELOPE_PREFIX) {
        let keys = broker_proof_v3::keys().map_err(|_| ContentError::new(503,"broker verification unavailable"))?;
        let actor=serde_json::json!({"tenant_id":scope.tenant_id,"user_id":scope.user_id,"session_id":scope.session_id,"execution_id":scope.execution_id,"run_id":scope.run_id,"tool_call_id":scope.tool_call_id,"arguments_hash":scope.arguments_hash});
        return broker_proof_v3::verify_with_keys(&keys,header,method,path,&broker_proof_v3::body_hash(body).map_err(|_|ContentError::new(401,"capability body invalid"))?,&actor,now_ms.max(chrono::Utc::now().timestamp_millis().max(0) as u64))
            .map_err(|_|ContentError::new(401,"capability proof invalid"));
    }
    if !broker_proof_v3::legacy_admitted(now_ms) { return Err(ContentError::new(401,"legacy broker proof not admitted")); }
    verify_capability_proof(secret,header,method,path,body,scope,now)
}
pub fn verify_capability_proof(
    secret: &[u8],
    header: &str,
    method: &str,
    path: &str,
    body: &Value,
    scope: &CapabilityScope,
    now: i64,
) -> Result<Value, ContentError> {
    let invalid = || ContentError::new(401, "capability proof invalid");
    if secret.len() < 32 {
        return Err(invalid());
    }
    let encoded = header
        .strip_prefix("v1.")
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or_else(invalid)?;
    let raw = URL_SAFE_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .map_err(|_| invalid())?;
    let mut envelope: Value = serde_json::from_slice(&raw).map_err(|_| invalid())?;
    let signature = envelope
        .as_object_mut()
        .ok_or_else(invalid)?
        .remove("signature")
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| s.len() == 64)
        .ok_or_else(invalid)?;
    let expected_fields = [
        "schema_version",
        "method",
        "path",
        "body_sha256",
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
        "expires_at",
        "nonce",
    ];
    let map = envelope.as_object().ok_or_else(invalid)?;
    if map.len() != expected_fields.len()
        || !expected_fields.iter().all(|k| map.contains_key(*k))
        || envelope["schema_version"] != "ai-platform-capability-proof/v1"
    {
        return Err(invalid());
    }
    for key in [
        "method",
        "path",
        "body_sha256",
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
        "nonce",
    ] {
        if string(&envelope, key).is_empty() {
            return Err(invalid());
        }
    }
    for key in [
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
        "nonce",
    ] {
        let v = string(&envelope, key);
        if v.chars().count() > 255 || v.chars().any(|c| (c as u32) < 32) {
            return Err(invalid());
        }
    }
    if string(&envelope, "nonce").chars().count() > 128
        || string(&envelope, "path").chars().count() > 2048
        || string(&envelope, "path").chars().any(|c| (c as u32) < 32)
        || !hex_id(string(&envelope, "body_sha256"), "", 64)
    {
        return Err(invalid());
    }
    let expiry = envelope["expires_at"].as_i64().ok_or_else(invalid)?;
    if expiry <= 0 || expiry <= now || expiry > now + 120 {
        return Err(invalid());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| invalid())?;
    mac.update(&canonical_json(&envelope)?);
    let signed = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if !constant_time_equal(&signature, &signed) {
        return Err(invalid());
    }
    let hash = sha256(&canonical_json(body)?);
    let method = method.trim().to_uppercase();
    for (k, v) in [
        ("method", method.as_str()),
        ("path", path),
        ("body_sha256", hash.as_str()),
        ("tenant_id", scope.tenant_id.as_str()),
        ("user_id", scope.user_id.as_str()),
        ("session_id", scope.session_id.as_str()),
        ("execution_id", scope.execution_id.as_str()),
        ("run_id", scope.run_id.as_str()),
    ] {
        if !constant_time_equal(string(&envelope, k), v) {
            return Err(invalid());
        }
    }
    Ok(envelope)
}
