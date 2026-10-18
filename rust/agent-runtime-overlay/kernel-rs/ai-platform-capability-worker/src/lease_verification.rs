//! Verify-only dispatch policy. No root/attenuation private key is accepted here.
use ai_platform_capability_contract::{
    CreateCapabilityExecutionRequestV2,
    lease_v3::{self as wire, CapabilityToken, PublicKeyring},
};
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn keys() -> Result<Option<PublicKeyring>, &'static str> {
    if std::env::var_os("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE").is_some() {
        return Err("consumer_private_key_forbidden");
    }
    let file = std::env::var_os("AI_PLATFORM_LEASE_VERIFY_KEYRING_FILE");
    let json = std::env::var("AI_PLATFORM_LEASE_VERIFY_KEYRING_JSON").ok();
    if file.is_some() && json.is_some() {
        return Err("lease_keyring_invalid");
    }
    let bytes = match (file, json) {
        (Some(path), None) => Some(std::fs::read(path).map_err(|_| "lease_keyring_invalid")?),
        (None, Some(text)) => Some(text.into_bytes()),
        _ => None,
    };
    let Some(bytes) = bytes else { return Ok(None) };
    if bytes.len() > 16_384 {
        return Err("lease_keyring_invalid");
    }
    let keys: PublicKeyring =
        serde_json::from_slice(&bytes).map_err(|_| "lease_keyring_invalid")?;
    keys.validate()?;
    Ok(Some(keys))
}
pub fn validate_configuration(legacy_secret: &[u8]) -> Result<(), &'static str> {
    if keys()?.is_some() {
        match std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref() {
            Ok("transition") => {
                if legacy_secret.len() < 32 || !legacy_window() {
                    return Err("legacy_lease_window_not_admitted");
                }
            }
            Ok("verify_only") | Err(_) => {
                if !legacy_secret.is_empty()
                    || std::env::var_os("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET").is_some()
                {
                    return Err("consumer_signing_secret_forbidden");
                }
            }
            _ => return Err("lease_trust_mode_invalid"),
        }
    } else {
        if !matches!(
            std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref(),
            Err(_) | Ok("legacy_hmac")
        ) {
            return Err("lease_public_key_missing");
        }
        if legacy_secret.len() < 32 {
            return Err("legacy_lease_secret_invalid");
        }
    }
    Ok(())
}
fn legacy_window() -> bool {
    let now = now();
    std::env::var("AI_PLATFORM_LEASE_HMAC_ACCEPT_UNTIL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .is_some_and(|expiry| now < expiry && expiry <= now.saturating_add(86_400_000))
}
pub fn verify(
    request: &CreateCapabilityExecutionRequestV2,
    legacy_secret: &[u8],
) -> Result<(), &'static str> {
    validate_configuration(legacy_secret)?;
    let public = keys()?;
    if request.lease.signature.starts_with(wire::CAPABILITY_PREFIX) {
        let public = public.ok_or("lease_public_key_missing")?;
        let token: CapabilityToken =
            wire::decode(wire::CAPABILITY_PREFIX, &request.lease.signature)?;
        let mut claims =
            serde_json::to_value(&request.lease).map_err(|_| "lease_claims_invalid")?;
        claims["signature"] = serde_json::json!("");
        token.verify_call(&public, &claims, &request.descriptor.schema_hash, now())
    } else {
        if public.is_some()
            && (std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref() != Ok("transition")
                || !legacy_window())
        {
            return Err("legacy_lease_not_admitted");
        }
        request
            .lease
            .verify_signature(legacy_secret)
            .map_err(|_| "legacy_lease_signature_invalid")
    }
}

pub fn verify_snapshot(
    request: &CreateCapabilityExecutionRequestV2,
    snapshot_id: &str,
) -> Result<(), &'static str> {
    if request.lease.signature.starts_with(wire::CAPABILITY_PREFIX) {
        let token: CapabilityToken =
            wire::decode(wire::CAPABILITY_PREFIX, &request.lease.signature)?;
        if token.root.claims["snapshot_id"].as_str() != Some(snapshot_id) {
            return Err("capability_ceiling_snapshot_mismatch");
        }
    }
    Ok(())
}

/// Service bootstrap has no HMAC/signing-key input. Legacy in-memory fixtures
/// retain their explicit test helpers; they cannot change this startup policy.
pub fn validate_production_configuration() -> Result<(), &'static str> {
    if std::env::var_os("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET").is_some()
        || matches!(
            std::env::var("AI_PLATFORM_LEASE_TRUST_MODE").as_deref(),
            Ok("transition" | "legacy_hmac")
        )
    {
        return Err("consumer_signing_secret_forbidden");
    }
    keys()?.ok_or("lease_public_key_missing")?;
    validate_configuration(&[])
}
