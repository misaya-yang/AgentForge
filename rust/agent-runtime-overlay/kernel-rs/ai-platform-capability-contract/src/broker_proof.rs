//! Broker proof V3 verification. Only the Gateway issues these short-lived,
//! body-bound envelopes. Consumers load public keys on each request so revoked
//! keys stop authorizing requests without restarting a worker.
pub use crate::lease_v3 as wire;
use serde_json::{Value, json};
pub fn body_hash(body: &Value) -> Result<String, &'static str> {
    use sha2::{Digest, Sha256};
    Ok(wire::hex(&Sha256::digest(wire::bytes(body)?)))
}
pub const PURPOSE: &str = "capability-broker-proof";
pub const SCHEMA: &str = "ai-platform-capability-proof/v3";
pub const ISSUER_PATH: &str = "/internal/v3/capability-broker/proofs";
static EXPLICIT_ENVIRONMENT: std::sync::OnceLock<std::collections::BTreeMap<String, String>> =
    std::sync::OnceLock::new();
pub fn configure_environment(
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<(), &'static str> {
    let selected = environment
        .iter()
        .filter(|(k, _)| {
            k.starts_with("AI_PLATFORM_BROKER_PROOF_")
                || k.starts_with("AI_PLATFORM_LEASE_")
                || k.starts_with("AI_PLATFORM_WORKLOAD_")
                || k.as_str() == "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE"
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(current) = EXPLICIT_ENVIRONMENT.get() {
        return if *current == selected {
            Ok(())
        } else {
            Err("broker_configuration_already_frozen")
        };
    }
    EXPLICIT_ENVIRONMENT
        .set(selected)
        .map_err(|_| "broker_configuration_already_frozen")
}
pub fn setting(name: &str) -> Result<String, std::env::VarError> {
    if let Some(environment) = EXPLICIT_ENVIRONMENT.get() {
        return environment
            .get(name)
            .cloned()
            .ok_or(std::env::VarError::NotPresent);
    }
    std::env::var(name)
}
pub fn required() -> bool {
    setting("AI_PLATFORM_BROKER_PROOF_MODE").as_deref() == Ok("verify_only")
        || setting("AI_PLATFORM_LEASE_TRUST_MODE").as_deref() == Ok("verify_only")
        || setting("AI_PLATFORM_LEASE_VERIFY_KEYRING_FILE").is_ok()
        || setting("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE").is_ok()
}
pub fn verify_only_required() -> bool {
    setting("AI_PLATFORM_BROKER_PROOF_MODE").as_deref() == Ok("verify_only")
        || setting("AI_PLATFORM_LEASE_TRUST_MODE").as_deref() == Ok("verify_only")
        || setting("AI_PLATFORM_WORKLOAD_MTLS_MODE").as_deref() == Ok("required")
}
pub fn deadline_admitted(now: u64, deadline: Option<u64>, verify_only: bool) -> bool {
    !verify_only && deadline.is_some_and(|end| now < end && end <= now.saturating_add(86_400_000))
}
pub fn legacy_admitted(now_ms: u64) -> bool {
    // The transition is explicit and bounded. A V3 verification failure never
    // enters this branch, even while the legacy window is open.
    !verify_only_required()
        && setting("AI_PLATFORM_BROKER_PROOF_MODE").as_deref() == Ok("transition")
        && setting("AI_PLATFORM_LEASE_TRUST_MODE").as_deref() != Ok("verify_only")
        && setting("AI_PLATFORM_BROKER_PROOF_HMAC_ACCEPT_UNTIL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|end| now_ms < end && end <= now_ms.saturating_add(86_400_000))
}
pub fn keys() -> Result<wire::PublicKeyring, &'static str> {
    let path = setting("AI_PLATFORM_LEASE_VERIFY_KEYRING_FILE")
        .map_err(|_| "broker_public_keyring_required")?;
    let bytes = std::fs::read(path).map_err(|_| "broker_public_keyring_invalid")?;
    if bytes.len() > 16_384 {
        return Err("broker_public_keyring_invalid");
    }
    let keys: wire::PublicKeyring =
        serde_json::from_slice(&bytes).map_err(|_| "broker_public_keyring_invalid")?;
    keys.validate()?;
    Ok(keys)
}
pub fn verify_with_keys(
    keys: &wire::PublicKeyring,
    header: &str,
    method: &str,
    path: &str,
    body_sha256: &str,
    scope: &Value,
    now_ms: u64,
) -> Result<Value, &'static str> {
    let e: wire::SignedEnvelope = wire::decode(wire::ENVELOPE_PREFIX, header)?;
    keys.verify(&e, PURPOSE, now_ms)?;
    let c = &e.claims;
    if c["schema_version"] != SCHEMA
        || c["method"] != method
        || c["path"] != path
        || c["body_sha256"] != body_sha256
        || e.expires_at_ms.saturating_sub(e.issued_at_ms) > 30_000
        || c["issued_at_ms"].as_u64() != Some(e.issued_at_ms)
        || c["expires_at_ms"].as_u64() != Some(e.expires_at_ms)
    {
        return Err("broker_proof_binding_invalid");
    }
    for field in [
        "tenant_id",
        "user_id",
        "session_id",
        "execution_id",
        "run_id",
    ] {
        if c[field].as_str().is_none_or(|v| v.is_empty()) || c[field] != scope[field] {
            return Err("broker_proof_scope_invalid");
        }
    }
    for field in ["tool_call_id", "arguments_hash"] {
        if scope[field].as_str().is_some_and(|v| !v.is_empty()) && c[field] != scope[field] {
            return Err("broker_proof_scope_invalid");
        }
    }
    // V1 callers consume this field for their absolute transport budget.
    let mut result = c.clone();
    result["expires_at"] = json!(e.expires_at_ms / 1000);
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_window_is_absolute_bounded_and_never_overrides_required() {
        assert!(deadline_admitted(100, Some(200), false));
        assert!(!deadline_admitted(200, Some(200), false));
        assert!(!deadline_admitted(100, Some(86_400_101), false));
        assert!(!deadline_admitted(100, Some(200), true));
        assert!(!deadline_admitted(100, None, false));
    }
    #[test]
    fn body_binding_is_sorted_recursive_and_retains_unicode_and_number_semantics() {
        let left = json!({"z":{"b":1.0,"a":"退款"},"a":[3,2,1]});
        let right: Value =
            serde_json::from_str(r#"{"a":[3,2,1],"z":{"a":"退款","b":1.0}}"#).unwrap();
        assert_eq!(body_hash(&left).unwrap(), body_hash(&right).unwrap());
        let mut changed = right;
        changed["z"]["b"] = json!(2.0);
        assert_ne!(body_hash(&left).unwrap(), body_hash(&changed).unwrap());
    }
    #[test]
    fn verifier_binds_body_route_actor_execution_and_key_revocation() {
        let seed =
            wire::unhex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
                .unwrap();
        let mut keys = wire::PublicKeyring {
            schema_version: "gateway-verification-keys/v2".into(),
            keys: vec![wire::PublicKey {
                key_id: "fixture".into(),
                public_key_hex: "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
                    .into(),
                not_before_ms: 1,
                not_after_ms: 100000,
                revoked: false,
            }],
        };
        let scope =
            json!({"tenant_id":"t","user_id":"u","session_id":"s","execution_id":"e","run_id":"r"});
        let mut c = scope.clone();
        c["schema_version"] = json!(SCHEMA);
        c["method"] = json!("POST");
        c["path"] = json!("/read");
        c["body_sha256"] = json!("body");
        c["issued_at_ms"] = json!(10);
        c["expires_at_ms"] = json!(1000);
        let e = wire::SignedEnvelope::issue(&seed, "fixture", PURPOSE, c, 10, 1000, &keys).unwrap();
        let token = wire::encode(wire::ENVELOPE_PREFIX, &e).unwrap();
        assert!(verify_with_keys(&keys, &token, "POST", "/read", "body", &scope, 20).is_ok());
        assert!(verify_with_keys(&keys, &token, "POST", "/write", "body", &scope, 20).is_err());
        assert!(verify_with_keys(&keys, &token, "POST", "/read", "changed", &scope, 20).is_err());
        for f in [
            "tenant_id",
            "user_id",
            "session_id",
            "execution_id",
            "run_id",
        ] {
            let mut s = scope.clone();
            s[f] = json!("other");
            assert!(verify_with_keys(&keys, &token, "POST", "/read", "body", &s, 20).is_err());
        }
        keys.keys[0].revoked = true;
        assert!(verify_with_keys(&keys, &token, "POST", "/read", "body", &scope, 20).is_err());
    }
}
