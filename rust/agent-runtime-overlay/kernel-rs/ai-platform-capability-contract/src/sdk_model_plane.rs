//! Shared private SDK model-plane envelope. No provider credential or endpoint.
use serde::{Deserialize, Serialize};
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPlane {
    pub schema: String,
    pub url: String,
    pub revocation_url: String,
    pub lease: String,
    pub run_id: String,
    pub binding_sha256: String,
    pub expires_at_epoch_ms: u64,
}
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
impl ModelPlane {
    pub fn validate(&self, expected_run: &str) -> Result<(), &'static str> {
        let url = reqwest::Url::parse(&self.url).map_err(|_| "sdk_model_plane_invalid")?;
        let revocation =
            reqwest::Url::parse(&self.revocation_url).map_err(|_| "sdk_model_plane_invalid")?;
        if self.schema != "sdk-model-plane/v1"
            || self.run_id != expected_run
            || uuid::Uuid::parse_str(&self.run_id).is_err()
            || self.binding_sha256.len() != 64
            || !self
                .binding_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.lease.len() != 64
            || !self
                .lease
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_url(&url, "/internal/v1/sdk-tools/models/execute")
            || !valid_url(&revocation, "/internal/v1/sdk-tools/models/revocations")
            || url.origin() != revocation.origin()
            || self.expires_at_epoch_ms <= now_ms()
            || self.expires_at_epoch_ms > now_ms().saturating_add(1_800_000)
        {
            return Err("sdk_model_plane_invalid");
        }
        Ok(())
    }
    pub fn validate_launch(&self, request: &serde_json::Value) -> Result<(), &'static str> {
        let run = request["capability_bridge"]["run_id"]
            .as_str()
            .ok_or("sdk_model_lease_required")?;
        self.validate(run)?;
        if self.binding_sha256 != launch_binding(request) {
            return Err("sdk_model_lease_scope_mismatch");
        }
        Ok(())
    }
    pub fn remaining(&self) -> Result<std::time::Duration, &'static str> {
        self.expires_at_epoch_ms
            .checked_sub(now_ms())
            .filter(|n| *n > 0)
            .map(std::time::Duration::from_millis)
            .ok_or("sdk_model_lease_expired")
    }
}
/// UTF-8 tuple avoids legacy Python/ascii fingerprint coercion and binds the
/// worker's durable session namespace to the Gateway-authenticated grant.
pub fn launch_binding(request: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let instructions = request["agent"]["instructions"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("You are a helpful assistant.");
    let tuple = serde_json::json!([
        request["tenant_id"],
        request["user_id"],
        request["service_id"],
        request["framework"],
        request["session_id"],
        request["request_id"],
        request["agent"]["model"],
        instructions,
        request["agent"]["max_turns"]
    ]);
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&tuple).expect("JSON tuple"))
    )
}
fn valid_url(url: &reqwest::Url, path: &str) -> bool {
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    (url.scheme() == "https" || url.scheme() == "http" && loopback)
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == path
}
#[cfg(test)]
mod tests {
    use super::*;
    fn plane() -> ModelPlane {
        ModelPlane {
            schema: "sdk-model-plane/v1".into(),
            url: "https://gateway.internal/internal/v1/sdk-tools/models/execute".into(),
            revocation_url: "https://gateway.internal/internal/v1/sdk-tools/models/revocations"
                .into(),
            lease: "a".repeat(64),
            run_id: uuid::Uuid::from_u128(1).to_string(),
            binding_sha256: "a".repeat(64),
            expires_at_epoch_ms: now_ms() + 60_000,
        }
    }
    #[test]
    fn authenticated_scope_binding_cannot_move_a_run_into_another_local_session() {
        let request = serde_json::json!({"tenant_id":"tenant","user_id":"user","service_id":"svc","framework":"openai_agents","session_id":"session","request_id":"request","agent":{"model":"model","instructions":"","max_turns":2},"capability_bridge":{"run_id":uuid::Uuid::from_u128(1).to_string()}});
        let mut plane = plane();
        plane.binding_sha256 = launch_binding(&request);
        assert!(plane.validate_launch(&request).is_ok());
        for field in [
            "tenant_id",
            "user_id",
            "service_id",
            "framework",
            "session_id",
            "request_id",
        ] {
            let mut changed = request.clone();
            changed[field] = "other".into();
            assert!(plane.validate_launch(&changed).is_err());
        }
    }
    #[test]
    fn model_lease_is_run_bound_short_lived_and_same_origin() {
        let p = plane();
        assert!(p.validate(&p.run_id).is_ok());
        for kind in 0..6 {
            let mut bad = p.clone();
            match kind {
                0 => bad.url = "https://api.openai.com/v1/responses".into(),
                1 => {
                    bad.revocation_url =
                        "https://other/internal/v1/sdk-tools/models/revocations".into()
                }
                2 => bad.expires_at_epoch_ms = 1,
                3 => bad.expires_at_epoch_ms = now_ms() + 1_800_001,
                4 => bad.lease = "provider-secret".into(),
                _ => {
                    bad.url = "http://gateway.internal/internal/v1/sdk-tools/models/execute".into()
                }
            };
            assert!(bad.validate(&p.run_id).is_err());
        }
        assert!(p.validate(&uuid::Uuid::from_u128(2).to_string()).is_err());
    }
}
