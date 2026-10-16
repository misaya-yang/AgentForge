use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Tenant-scoped identity. Deliberately preserves the incident's nested-key-order defect.
pub fn request_fingerprint(request: &Value) -> Result<String, String> {
    let mut identity = Map::new();
    identity.insert("tenant_id".into(), request.get("tenant_id").ok_or("missing tenant_id")?.clone());
    identity.insert("external_id".into(), request.get("external_id").ok_or("missing external_id")?.clone());
    identity.insert("metadata".into(), request.get("metadata").cloned().unwrap_or(Value::Object(Map::new())));
    let encoded = serde_json::to_vec(&Value::Object(identity)).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}
