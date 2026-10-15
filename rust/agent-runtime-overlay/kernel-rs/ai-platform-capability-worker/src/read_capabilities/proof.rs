//! HMAC capability-proof signing shared with downstream platform services.

use std::collections::BTreeMap;

use ai_platform_capability_contract::canonical_json_hash;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::Mac;
use serde_json::{Value, json};

#[cfg(test)]
use super::web::is_public_ip;
#[cfg(test)]
use super::web::resolved_peer_ip;
use super::{
    CAPABILITY_PROOF_SCHEMA_VERSION, CAPABILITY_PROOF_TTL_SECONDS, HmacSha256,
    MIN_PROOF_SECRET_BYTES, ReadCapabilityContext, ReadCapabilityError,
};

/// Sign the exact downstream request that will be sent to a trusted platform
/// service. The envelope and its body hash intentionally match the Python
/// `capability_proof.py` implementation byte-for-byte.
pub(super) struct CapabilityProofInput<'a> {
    pub(super) method: &'a str,
    pub(super) path: &'a str,
    pub(super) body: &'a Value,
    pub(super) context: &'a ReadCapabilityContext,
    pub(super) now: u64,
    pub(super) nonce: String,
}

pub(super) fn sign_capability_proof(
    secret: &str,
    input: CapabilityProofInput<'_>,
) -> Result<String, ReadCapabilityError> {
    if secret.len() < MIN_PROOF_SECRET_BYTES || input.now == 0 || input.nonce.is_empty() {
        return Err(ReadCapabilityError::Configuration);
    }
    let expires_at = input
        .now
        .checked_add(CAPABILITY_PROOF_TTL_SECONDS)
        .ok_or(ReadCapabilityError::Configuration)?;
    let body_sha256 = canonical_json_hash(input.body)
        .map_err(|_| ReadCapabilityError::Configuration)?
        .strip_prefix("sha256:")
        .map(str::to_owned)
        .ok_or(ReadCapabilityError::Configuration)?;

    let mut unsigned = BTreeMap::<String, Value>::new();
    unsigned.insert("body_sha256".to_string(), Value::String(body_sha256));
    unsigned.insert(
        "execution_id".to_string(),
        Value::String(input.context.execution_id.clone()),
    );
    unsigned.insert("expires_at".to_string(), json!(expires_at));
    unsigned.insert(
        "method".to_string(),
        Value::String(input.method.trim().to_ascii_uppercase()),
    );
    unsigned.insert("nonce".to_string(), Value::String(input.nonce));
    unsigned.insert(
        "path".to_string(),
        Value::String(input.path.trim().to_string()),
    );
    unsigned.insert(
        "run_id".to_string(),
        Value::String(input.context.run_id.clone()),
    );
    unsigned.insert(
        "schema_version".to_string(),
        Value::String(CAPABILITY_PROOF_SCHEMA_VERSION.to_string()),
    );
    unsigned.insert(
        "session_id".to_string(),
        Value::String(input.context.session_id.clone()),
    );
    unsigned.insert(
        "tenant_id".to_string(),
        Value::String(input.context.tenant_id.clone()),
    );
    unsigned.insert(
        "user_id".to_string(),
        Value::String(input.context.user_id.clone()),
    );

    let unsigned_bytes =
        serde_json::to_vec(&unsigned).map_err(|_| ReadCapabilityError::Configuration)?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| ReadCapabilityError::Configuration)?;
    mac.update(&unsigned_bytes);
    let signature = hex::encode(mac.finalize().into_bytes());

    let mut envelope = unsigned;
    envelope.insert("signature".to_string(), Value::String(signature));
    let envelope_bytes =
        serde_json::to_vec(&envelope).map_err(|_| ReadCapabilityError::Configuration)?;
    Ok(format!("v1.{}", URL_SAFE_NO_PAD.encode(envelope_bytes)))
}

#[cfg(test)]
mod capability_proof_tests {
    use std::collections::BTreeSet;

    use super::{CapabilityProofInput, ReadCapabilityContext, sign_capability_proof};
    use serde_json::json;

    #[test]
    fn pinned_public_peer_survives_missing_remote_addr() {
        let pinned = "1.1.1.1:443".parse::<std::net::SocketAddr>().unwrap();
        assert_eq!(
            super::resolved_peer_ip(None, &[pinned]),
            Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(1, 1, 1, 1)))
        );
    }

    #[test]
    fn observed_private_peer_is_never_masked_by_a_public_pin() {
        // The connection actually landed on loopback while the pin list is
        // public: the observation must pass through so the caller's
        // `SsrfBlocked` branch fires, instead of being overwritten by the pin.
        let pinned = "1.1.1.1:443".parse::<std::net::SocketAddr>().unwrap();
        assert_eq!(
            super::resolved_peer_ip(
                Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
                &[pinned],
            ),
            Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        );
    }

    #[test]
    fn ipv4_mapped_and_transition_v6_are_not_public() {
        // Attacker-controlled AAAA records that embed a private IPv4 must not
        // read as public (web_fetch URLs are model-influenced).
        use std::net::IpAddr;
        use std::net::Ipv6Addr;
        let mapped_loopback = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001); // ::ffff:127.0.0.1
        let mapped_metadata = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xa9fe, 0xa9fe); // ::ffff:169.254.169.254
        let compat_loopback = Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0x7f00, 0x0001); // ::127.0.0.1
        let sixto4 = Ipv6Addr::new(0x2002, 0x7f00, 0x0001, 0, 0, 0, 0, 0); // 2002:7f00:1::
        assert!(!super::is_public_ip(IpAddr::V6(mapped_loopback)));
        assert!(!super::is_public_ip(IpAddr::V6(mapped_metadata)));
        assert!(!super::is_public_ip(IpAddr::V6(compat_loopback)));
        assert!(!super::is_public_ip(IpAddr::V6(sixto4)));
        // A genuine global IPv6 address stays public.
        assert!(super::is_public_ip(IpAddr::V6(Ipv6Addr::new(
            0x2606, 0x4700, 0, 0, 0, 0, 0, 0x6806
        ))));
    }

    #[test]
    fn matches_python_capability_proof_fixture() {
        let body = json!({"query": "退款", "top_k": 5, "threshold": 0.0});
        let context = ReadCapabilityContext {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
            execution_id: "exec_01".to_string(),
            tool_call_id: "tool-call-01".to_string(),
            run_id: "run_01".to_string(),
            capability_revision: 1,
            bound_dataset_ids: BTreeSet::new(),
            connector_binding: None,
        };
        let header = sign_capability_proof(
            "capability-proof-secret-012345678901234567890123",
            CapabilityProofInput {
                method: "post",
                path: " /internal/v2/capabilities/knowledge/docs/retrieve ",
                body: &body,
                context: &context,
                now: 1_700_000_000,
                nonce: "nonce-fixed-0123456789".to_string(),
            },
        )
        .expect("fixture proof should sign");
        assert_eq!(
            header,
            "v1.eyJib2R5X3NoYTI1NiI6ImU2NTk3MGI3YzM2NWE4NmI2YzZkZGVjZDNhODllOGRlMzZkMzlhNzE5YTlhYjFmNGMwZGQ0YWI0MmMwNjYxMmEiLCJleGVjdXRpb25faWQiOiJleGVjXzAxIiwiZXhwaXJlc19hdCI6MTcwMDAwMDAzMCwibWV0aG9kIjoiUE9TVCIsIm5vbmNlIjoibm9uY2UtZml4ZWQtMDEyMzQ1Njc4OSIsInBhdGgiOiIvaW50ZXJuYWwvdjIvY2FwYWJpbGl0aWVzL2tub3dsZWRnZS9kb2NzL3JldHJpZXZlIiwicnVuX2lkIjoicnVuXzAxIiwic2NoZW1hX3ZlcnNpb24iOiJhaS1wbGF0Zm9ybS1jYXBhYmlsaXR5LXByb29mL3YxIiwic2Vzc2lvbl9pZCI6InNlc3Npb24tYSIsInNpZ25hdHVyZSI6IjUyNjBhNjExOTQyNTAxMzg1ODY2YmI0MTc1OGRlNjQ5OGY4NjE1MjdlYjNiNTExZTdkNWRiYmE5M2I4NjlhZmEiLCJ0ZW5hbnRfaWQiOiJ0ZW5hbnQtYSIsInVzZXJfaWQiOiJ1c2VyLWEifQ"
        );
    }
}
