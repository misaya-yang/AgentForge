//! Outbound-only HTTPS adapter. No redirects, downgrade, body credentials or POST replay.
use crate::{Error, Result, canonical, envelope::PlatformVerifier, signing::PinnedEd25519Verifier};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use url::Url;
pub const PROTOCOL_VERSION: &str = "ai-platform.local-node.v2";
pub const CONTROL_PATH: &str = "/api/v1/local-nodes/control";
pub struct Credential {
    pub device_id: String,
    secret: String,
    pub expires_at: f64,
}
impl Credential {
    pub fn new(device_id: String, secret: String, expires_at: f64) -> Result<Self> {
        if device_id.is_empty()
            || secret.is_empty()
            || secret.len() > 8192
            || !expires_at.is_finite()
            || expires_at <= crate::now()
        {
            return Err(Error::Protocol);
        }
        Ok(Self {
            device_id,
            secret,
            expires_at,
        })
    }
}
#[derive(Serialize)]
pub struct ExchangeRequest {
    pub protocol_version: String,
    pub device_id: String,
    pub kind: String,
    pub receipts: Vec<crate::outbox::Receipt>,
    pub doctor: Value,
    pub sent_at: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeResponse {
    pub protocol_version: String,
    pub accepted_through_sequence: u64,
    pub commands: Vec<Value>,
}
pub struct HttpsTransport {
    http: reqwest::Client,
    endpoint: Url,
}
impl HttpsTransport {
    pub fn new(endpoint: &str, timeout: Duration) -> Result<Self> {
        Self::new_with_ca(endpoint, timeout, None)
    }

    /// Use a single operator-provided CA as the only trust root when supplied.
    /// This supports private deployments without changing system trust or
    /// disabling certificate/hostname verification.
    pub fn new_with_ca(endpoint: &str, timeout: Duration, ca_pem: Option<&[u8]>) -> Result<Self> {
        let endpoint = Url::parse(endpoint).map_err(|_| Error::Protocol)?;
        if endpoint.scheme() != "https"
            || endpoint.path() != CONTROL_PATH
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || timeout.is_zero()
            || timeout > Duration::from_secs(120)
        {
            return Err(Error::Denied("HTTPS endpoint required"));
        }
        let builder = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout);
        let builder = if let Some(ca_pem) = ca_pem {
            let certificate = reqwest::Certificate::from_pem(ca_pem)
                .map_err(|_| Error::Denied("TLS CA certificate invalid"))?;
            builder.tls_certs_only([certificate])
        } else {
            builder
        };
        let http = builder.build().map_err(|_| Error::Transport)?;
        Ok(Self { http, endpoint })
    }
    pub async fn post(&self, payload: &Value, credential: Option<&Credential>) -> Result<Value> {
        let mut request = self
            .http
            .post(self.endpoint.clone())
            .header("Accept", "application/json")
            .header("User-Agent", "AI-Platform-Local-Node/0.1")
            .json(payload);
        if let Some(c) = credential {
            if crate::now() >= c.expires_at {
                return Err(Error::Denied("device credential expired"));
            }
            request = request.header("Authorization", format!("Device {}", c.secret));
        }
        let mut response = request.send().await.map_err(|_| Error::Transport)?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .map(str::trim)
                != Some("application/json")
        {
            return Err(Error::Protocol);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(Error::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
        if !v.is_object() {
            return Err(Error::Protocol);
        }
        Ok(v)
    }
    pub async fn exchange(
        &self,
        credential: &Credential,
        request: &ExchangeRequest,
    ) -> Result<ExchangeResponse> {
        if request.device_id != credential.device_id
            || request.protocol_version != PROTOCOL_VERSION
            || request.kind != "heartbeat"
            || !request.sent_at.is_finite()
        {
            return Err(Error::Protocol);
        }
        let value = self
            .post(
                &serde_json::to_value(request).map_err(|_| Error::Protocol)?,
                Some(credential),
            )
            .await?;
        let response: ExchangeResponse =
            serde_json::from_value(value).map_err(|_| Error::Protocol)?;
        if response.protocol_version != PROTOCOL_VERSION
            || response.commands.len() > 100
            || response.commands.iter().any(|v| !v.is_object())
        {
            return Err(Error::Protocol);
        }
        Ok(response)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        envelope::PlatformVerifier, signing::Ed25519PairingSigner, transport::PairingSigner,
    };
    #[test]
    fn rejects_downgrade_credentials_and_redirect_routes() {
        for url in [
            "http://127.0.0.1/",
            "wss://example.com/",
            "https://u:p@example.com/",
            "https://example.com/?token=x",
            "https://example.com/wrong-control-path",
        ] {
            assert!(HttpsTransport::new(url, Duration::from_secs(20)).is_err());
        }
    }

    #[test]
    fn v2_pairing_response_must_bind_challenge_device_credential_and_pinned_key() {
        let signer = Ed25519PairingSigner::from_identity_secret(&[11; 32]).unwrap();
        let request = PairingRequest {
            challenge_id: "challenge-1".into(),
            user_code: "redacted-test-code".into(),
            device_id: "device-1".into(),
            display_name: "fixture".into(),
            platform: "macos".into(),
            node_version: "test".into(),
            capability_claims: vec!["file.read".into()],
            permission_snapshot_digest: "a".repeat(64),
        };
        let mut response = PairingResponse {
            protocol_version: PROTOCOL_VERSION.into(),
            challenge_id: request.challenge_id.clone(),
            device_id: request.device_id.clone(),
            credential: "opaque-synthetic-credential".into(),
            expires_at: crate::now() + 60.0,
            platform_key_id: "fixture-platform-key".into(),
            platform_signature: String::new(),
        };
        response.platform_signature = signer
            .sign(canonical(&pairing_response_payload(&response)).as_bytes())
            .unwrap();
        let verifier = PinnedEd25519Verifier::new([(
            response.platform_key_id.clone(),
            signer.public_key().into(),
        )])
        .unwrap();

        assert_eq!(
            verify_pairing_response(response, &request, &verifier)
                .unwrap()
                .device_id,
            request.device_id
        );

        let mut wrong_challenge = PairingResponse {
            protocol_version: PROTOCOL_VERSION.into(),
            challenge_id: "another-challenge".into(),
            device_id: request.device_id.clone(),
            credential: "opaque-synthetic-credential".into(),
            expires_at: crate::now() + 60.0,
            platform_key_id: "fixture-platform-key".into(),
            platform_signature: String::new(),
        };
        wrong_challenge.platform_signature = signer
            .sign(canonical(&pairing_response_payload(&wrong_challenge)).as_bytes())
            .unwrap();
        assert!(matches!(
            verify_pairing_response(wrong_challenge, &request, &verifier),
            Err(Error::Protocol)
        ));
    }
}

pub trait PairingSigner: Send + Sync {
    fn algorithm(&self) -> &str;
    fn public_key(&self) -> &str;
    fn sign(&self, payload: &[u8]) -> Result<String>;
}
#[derive(Serialize)]
pub struct PairingRequest {
    pub challenge_id: String,
    pub user_code: String,
    pub device_id: String,
    pub display_name: String,
    pub platform: String,
    pub node_version: String,
    pub capability_claims: Vec<String>,
    pub permission_snapshot_digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingResponse {
    protocol_version: String,
    challenge_id: String,
    device_id: String,
    credential: String,
    expires_at: f64,
    platform_key_id: String,
    platform_signature: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredential {
    protocol_version: String,
    device_id: String,
    credential: String,
    expires_at: f64,
}
fn pairing_response_payload(response: &PairingResponse) -> Value {
    serde_json::json!({
        "protocol_version":response.protocol_version.as_str(),
        "challenge_id":response.challenge_id.as_str(),
        "device_id":response.device_id.as_str(),
        "credential":response.credential.as_str(),
        "expires_at":response.expires_at,
        "platform_key_id":response.platform_key_id.as_str(),
    })
}
fn verify_pairing_response(
    response: PairingResponse,
    request: &PairingRequest,
    platform_verifier: &PinnedEd25519Verifier,
) -> Result<Credential> {
    if response.protocol_version != PROTOCOL_VERSION
        || response.challenge_id != request.challenge_id
        || response.device_id != request.device_id
        || !platform_verifier.verify(
            &response.platform_key_id,
            canonical(&pairing_response_payload(&response)).as_bytes(),
            &response.platform_signature,
        )
    {
        return Err(Error::Protocol);
    }
    Credential::new(response.device_id, response.credential, response.expires_at)
}
impl HttpsTransport {
    pub async fn redeem_pairing(
        &self,
        mut request: PairingRequest,
        signer: &dyn PairingSigner,
        store: &dyn crate::credentials::SecureCredentialStore,
        reference: &str,
        platform_verifier: &PinnedEd25519Verifier,
    ) -> Result<Credential> {
        let valid = |s: &str, max: usize| {
            !s.is_empty() && s.chars().count() <= max && !s.chars().any(|c| c < ' ')
        };
        if !store.ready()
            || store.load(reference)?.is_some()
            || !valid(&request.challenge_id, 4096)
            || !valid(&request.user_code, 4096)
            || !valid(&request.device_id, 4096)
            || !valid(&request.display_name, 80)
            || !valid(&request.node_version, 64)
            || !["macos", "windows", "linux"].contains(&request.platform.as_str())
            || request.capability_claims.is_empty()
            || request.capability_claims.len() > 32
            || request.permission_snapshot_digest.len() != 64
            || !request
                .permission_snapshot_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Denied("invalid pairing scope"));
        }
        request.capability_claims.sort();
        if request.capability_claims.windows(2).any(|w| w[0] == w[1]) {
            return Err(Error::Protocol);
        }
        let mut value = serde_json::to_value(&request).map_err(|_| Error::Protocol)?;
        let o = value.as_object_mut().unwrap();
        o.insert(
            "protocol_version".into(),
            serde_json::json!(PROTOCOL_VERSION),
        );
        o.insert("kind".into(), serde_json::json!("pairing_redeem"));
        o.insert(
            "proof_algorithm".into(),
            serde_json::json!(signer.algorithm()),
        );
        o.insert(
            "proof_public_key".into(),
            serde_json::json!(signer.public_key()),
        );
        let mut preproof = value.clone();
        preproof.as_object_mut().unwrap().remove("user_code");
        preproof["user_code_digest"] =
            serde_json::json!(crate::sha256(request.user_code.as_bytes()));
        let redemption = format!("sha256:{}", crate::digest(&preproof));
        let proof = signer.sign(
            crate::canonical(&serde_json::json!({"redemption_digest":redemption})).as_bytes(),
        )?;
        value["device_proof"] = serde_json::json!(proof);
        let response: PairingResponse =
            serde_json::from_value(self.post(&value, None).await?).map_err(|_| Error::Protocol)?;
        let credential = verify_pairing_response(response, &request, platform_verifier)?;
        let saved = serde_json::json!({
            "protocol_version":PROTOCOL_VERSION,
            "device_id":credential.device_id.as_str(),
            "credential":credential.secret.as_str(),
            "expires_at":credential.expires_at
        });
        store.store(reference, crate::canonical(&saved).as_bytes())?;
        Ok(credential)
    }
}
pub fn load_credential(
    store: &dyn crate::credentials::SecureCredentialStore,
    reference: &str,
    device: &str,
) -> Result<Option<Credential>> {
    let Some(raw) = store.load(reference)? else {
        return Ok(None);
    };
    if raw.len() > 16384 {
        return Err(Error::Protocol);
    }
    let response: StoredCredential = serde_json::from_slice(&raw).map_err(|_| Error::Protocol)?;
    if response.protocol_version != PROTOCOL_VERSION || response.device_id != device {
        return Err(Error::Denied("credential identity"));
    }
    if response.expires_at <= crate::now() {
        return Ok(None);
    }
    Ok(Some(Credential::new(
        response.device_id,
        response.credential,
        response.expires_at,
    )?))
}
