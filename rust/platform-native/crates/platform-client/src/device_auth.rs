//! RFC 8628 public-client login. Secrets only cross authenticated HTTPS and the
//! native OS vault; refresh is submitted once, never replayed after uncertainty.
use crate::{Client, Error};
use ai_platform_local_node::credentials::{SecureCredentialStore, native_secure_store};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const VAULT_SERVICE: &str = "ai-platform.public-client.oauth.v1";
pub const AUTHORIZE_PATH: &str = "/api/v1/auth/device/authorize";
pub const TOKEN_PATH: &str = "/api/v1/auth/device/token";
pub const REVOKE_PATH: &str = "/api/v1/auth/device/revoke";
const MAX_RESPONSE: usize = 64 * 1024;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

#[derive(Deserialize)]
pub struct DeviceChallenge {
    device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}
fn default_interval() -> u64 {
    5
}
#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: String,
    token_type: String,
    scope: String,
    expires_in: u64,
    tenant_id: String,
    user_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSession {
    version: u32,
    #[serde(default)]
    refresh_in_flight: bool,
    origin: String,
    client_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    tenant_id: String,
    user_id: String,
}
pub struct AccessToken {
    pub expires_at: u64,
    pub token: String,
    pub tenant_id: String,
    pub user_id: String,
}
pub struct DeviceLogin {
    client: Client,
    client_id: String,
    reference: String,
    vault: Box<dyn SecureCredentialStore>,
}
impl DeviceLogin {
    pub fn new(origin: &str, client_id: &str) -> Result<Self, Error> {
        let vault = native_secure_store(VAULT_SERVICE).map_err(|_| Error::Authentication)?;
        Self::with_vault(origin, client_id, vault)
    }
    pub fn with_vault(
        origin: &str,
        client_id: &str,
        vault: Box<dyn SecureCredentialStore>,
    ) -> Result<Self, Error> {
        if !matches!(client_id, "agentforge-cli") || !vault.ready() {
            return Err(Error::Authentication);
        }
        let client = Client::new(origin)?;
        let reference = format!(
            "oauth_{:x}",
            Sha256::digest(format!("{}\0{client_id}", client.origin).as_bytes())
        );
        Ok(Self {
            client,
            client_id: client_id.into(),
            reference,
            vault,
        })
    }
    async fn lock(&self) -> Result<ai_platform_local_node::credential_lock::CredentialLock, Error> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(lock) =
                ai_platform_local_node::credential_lock::try_acquire(&self.reference)
                    .map_err(|_| Error::Authentication)?
            {
                return Ok(lock);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Authentication);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Result<Value, Error> {
        let body = serde_json::to_value(
            fields
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        )
        .map_err(|_| Error::Request)?;
        // Use generated wire models while retaining this crate's single transport
        // and the public-client secret/unknown-outcome policy.
        let body = match path {
            AUTHORIZE_PATH => serde_json::to_value(
                serde_json::from_value::<
                    crate::generated::ApiPostApiApiV1AuthDeviceAuthorizeRequestApiBody,
                >(body)
                .map_err(|_| Error::Request)?,
            ),
            TOKEN_PATH => serde_json::to_value(
                serde_json::from_value::<
                    crate::generated::ApiPostApiApiV1AuthDeviceTokenRequestApiBody,
                >(body)
                .map_err(|_| Error::Request)?,
            ),
            REVOKE_PATH => serde_json::to_value(
                serde_json::from_value::<
                    crate::generated::ApiPostApiApiV1AuthDeviceRevokeRequestApiBody,
                >(body)
                .map_err(|_| Error::Request)?,
            ),
            _ => return Err(Error::Request),
        }
        .map_err(|_| Error::Request)?;
        let mut response = self
            .client
            .http
            .post(self.client.origin.join(path).map_err(|_| Error::Origin)?)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|_| Error::UnknownOutcome)?;
        let status = response.status();
        if status.as_u16() == 204 {
            return Ok(json!({}));
        }
        if response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            != Some("application/json")
        {
            return Err(Error::Response);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::UnknownOutcome)? {
            if bytes.len() + chunk.len() > MAX_RESPONSE {
                return Err(Error::Response);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Response)?;
        if !value.is_object() {
            return Err(Error::Response);
        }
        if !status.is_success() && !matches!(status.as_u16(), 400 | 401) {
            return Err(Error::Status(status.as_u16()));
        }
        if !status.is_success() && value.get("error").and_then(Value::as_str).is_none() {
            return Err(Error::Authentication);
        }
        Ok(value)
    }
    pub fn verifier() -> String {
        (0..4)
            .map(|_| uuid::Uuid::new_v4().simple().to_string())
            .collect()
    }
    pub async fn begin(&self, verifier: &str) -> Result<DeviceChallenge, Error> {
        if !(43..=128).contains(&verifier.len())
            || !verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        {
            return Err(Error::Request);
        }
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let value = self
            .post(
                AUTHORIZE_PATH,
                &[
                    ("client_id", &self.client_id),
                    ("scope", "platform"),
                    ("code_challenge", &challenge),
                    ("code_challenge_method", "S256"),
                ],
            )
            .await?;
        let challenge: DeviceChallenge =
            serde_json::from_value(value).map_err(|_| Error::Authentication)?;
        let uri = Client::new(
            &url::Url::parse(&challenge.verification_uri)
                .map_err(|_| Error::Response)?
                .origin()
                .ascii_serialization(),
        )?;
        if uri.origin.origin() != self.client.origin.origin()
            || !bounded(&challenge.device_code, 4096)
            || !bounded(&challenge.user_code, 64)
            || !(1..=900).contains(&challenge.expires_in)
            || !(1..=300).contains(&challenge.interval)
        {
            return Err(Error::Response);
        }
        let link = url::Url::parse(&challenge.verification_uri).map_err(|_| Error::Response)?;
        if !link.username().is_empty() || link.password().is_some() || link.fragment().is_some() {
            return Err(Error::Response);
        }
        Ok(challenge)
    }
    pub async fn complete(
        &self,
        challenge: DeviceChallenge,
        verifier: &str,
    ) -> Result<Value, Error> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(challenge.expires_in);
        let mut interval = challenge.interval;
        loop {
            if tokio::time::Instant::now() + Duration::from_secs(interval) >= deadline {
                return Err(Error::Authentication);
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let value = tokio::time::timeout_at(
                deadline,
                self.post(
                    TOKEN_PATH,
                    &[
                        ("client_id", &self.client_id),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("device_code", &challenge.device_code),
                        ("code_verifier", verifier),
                    ],
                ),
            )
            .await
            .map_err(|_| Error::Authentication)??;
            match value.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval = (interval + 5).min(300);
                    continue;
                }
                Some(_) => return Err(Error::Authentication),
                None => {
                    let _lock = self.lock().await?;
                    return self.save_reply(value, None);
                }
            }
        }
    }
    fn load(&self) -> Result<Option<StoredSession>, Error> {
        let Some(bytes) = self
            .vault
            .load(&self.reference)
            .map_err(|_| Error::Authentication)?
        else {
            return Ok(None);
        };
        if bytes.len() > 16384 {
            return Err(Error::Authentication);
        }
        let stored: StoredSession =
            serde_json::from_slice(&bytes).map_err(|_| Error::Authentication)?;
        if stored.version != 1
            || stored.origin != self.client.origin.as_str()
            || stored.client_id != self.client_id
            || !bounded(&stored.access_token, 8192)
            || !bounded(&stored.refresh_token, 4096)
            || !bounded(&stored.tenant_id, 255)
            || !bounded(&stored.user_id, 255)
        {
            return Err(Error::Authentication);
        }
        Ok(Some(stored))
    }
    fn save_reply(&self, value: Value, expected: Option<&StoredSession>) -> Result<Value, Error> {
        let reply: TokenReply = serde_json::from_value(value).map_err(|_| Error::Authentication)?;
        if reply.scope != "platform"
            || !reply.token_type.eq_ignore_ascii_case("bearer")
            || !bounded(&reply.access_token, 8192)
            || !bounded(&reply.refresh_token, 4096)
            || !bounded(&reply.tenant_id, 255)
            || !bounded(&reply.user_id, 255)
            || !(1..=86400).contains(&reply.expires_in)
            || expected
                .is_some_and(|old| old.tenant_id != reply.tenant_id || old.user_id != reply.user_id)
        {
            return Err(Error::Authentication);
        }
        let stored = StoredSession {
            version: 1,
            refresh_in_flight: false,
            origin: self.client.origin.to_string(),
            client_id: self.client_id.clone(),
            access_token: reply.access_token,
            refresh_token: reply.refresh_token,
            expires_at: now().checked_add(reply.expires_in).ok_or(Error::Response)?,
            tenant_id: reply.tenant_id,
            user_id: reply.user_id,
        };
        self.vault
            .store(
                &self.reference,
                &serde_json::to_vec(&stored).map_err(|_| Error::Response)?,
            )
            .map_err(|_| Error::Authentication)?;
        Ok(
            json!({"authenticated":true,"tenant_id":stored.tenant_id,"user_id":stored.user_id,"expires_at":stored.expires_at}),
        )
    }
    pub fn status(&self) -> Result<Value, Error> {
        Ok(match self.load()? {
            Some(s) => {
                json!({"authenticated":s.expires_at>now() && !s.refresh_in_flight,"reauthentication_required":s.refresh_in_flight,"tenant_id":s.tenant_id,"user_id":s.user_id,"expires_at":s.expires_at})
            }
            None => json!({"authenticated":false}),
        })
    }
    pub async fn access_token(&self) -> Result<AccessToken, Error> {
        let _lock = self.lock().await?;
        let mut session = self.load()?.ok_or(Error::Authentication)?;
        if session.refresh_in_flight {
            return Err(Error::Authentication);
        }
        if session.expires_at <= now() + 30 {
            // Persist intent before a rotating refresh. A lost response or crash
            // requires explicit login; subsequent processes cannot replay it.
            session.refresh_in_flight = true;
            self.vault
                .store(
                    &self.reference,
                    &serde_json::to_vec(&session).map_err(|_| Error::Response)?,
                )
                .map_err(|_| Error::Authentication)?;
            let reply = self
                .post(
                    TOKEN_PATH,
                    &[
                        ("client_id", &self.client_id),
                        ("grant_type", "refresh_token"),
                        ("refresh_token", &session.refresh_token),
                    ],
                )
                .await?;
            if reply.get("error").is_some() {
                self.vault
                    .remove(&self.reference)
                    .map_err(|_| Error::Authentication)?;
                return Err(Error::Authentication);
            }
            self.save_reply(reply, Some(&session))?;
            session = self.load()?.ok_or(Error::Authentication)?;
        }
        Ok(AccessToken {
            expires_at: session.expires_at,
            token: session.access_token,
            tenant_id: session.tenant_id,
            user_id: session.user_id,
        })
    }
    pub async fn logout(&self) -> Result<Value, Error> {
        let _lock = self.lock().await?;
        if let Some(session) = self.load()? {
            let outcome = self
                .post(REVOKE_PATH, &[("refresh_token", &session.refresh_token)])
                .await;
            // Local deletion always occurs, including a lost revoke response.
            self.vault
                .remove(&self.reference)
                .map_err(|_| Error::Authentication)?;
            let value = outcome?;
            if value.get("error").is_some() {
                return Err(Error::Authentication);
            }
            return Ok(json!({"authenticated":false,"revoked":true}));
        }
        Ok(json!({"authenticated":false,"revoked":false,"revocation":"no_local_session"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Vault(Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>);
    impl SecureCredentialStore for Vault {
        fn ready(&self) -> bool {
            true
        }
        fn load(&self, key: &str) -> ai_platform_local_node::Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn store(&self, key: &str, value: &[u8]) -> ai_platform_local_node::Result<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        fn remove(&self, key: &str) -> ai_platform_local_node::Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }
    fn reply(tenant: &str) -> Value {
        json!({"token_type":"Bearer","scope":"platform","access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_in":60,"tenant_id":tenant,"user_id":"user"})
    }
    #[test]
    fn vault_is_bound_to_origin_client_and_tenant_on_rotation() {
        let vault = Vault::default();
        let login = DeviceLogin::with_vault(
            "https://gateway.test",
            "agentforge-cli",
            Box::new(vault.clone()),
        )
        .unwrap();
        login.save_reply(reply("tenant-a"), None).unwrap();
        let old = login.load().unwrap().unwrap();
        assert!(login.save_reply(reply("tenant-b"), Some(&old)).is_err());
        assert!(
            DeviceLogin::with_vault(
                "https://other.test",
                "agentforge-cli",
                Box::new(vault.clone())
            )
            .unwrap()
            .load()
            .unwrap()
            .is_none()
        );
        assert!(
            DeviceLogin::with_vault("https://gateway.test", "ai-gateway-cli", Box::new(vault))
                .is_err()
        );
    }
    #[test]
    fn pkce_and_vault_reject_plaintext_or_unavailable_identity() {
        assert_eq!(DeviceLogin::verifier().len(), 128);
        assert!(
            DeviceLogin::with_vault(
                "http://remote.test",
                "agentforge-cli",
                Box::new(Vault::default())
            )
            .is_err()
        );
        assert!(
            DeviceLogin::with_vault(
                "https://gateway.test",
                "unknown",
                Box::new(Vault::default())
            )
            .is_err()
        );
        assert!(
            DeviceLogin::with_vault(
                "https://gateway.test",
                "agentforge-cli",
                Box::new(ai_platform_local_node::credentials::UnavailableStore)
            )
            .is_err()
        );
    }
    async fn http_fixture(
        mut replies: Vec<(u16, Value)>,
    ) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let captured = Arc::new(Mutex::new(Vec::new()));
        let record = captured.clone();
        let origin_copy = origin.clone();
        let server = tokio::spawn(async move {
            for (status, mut value) in replies.drain(..) {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (end, length) = loop {
                    let mut chunk = [0u8; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() < 16384);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8(bytes[..end].to_vec())
                            .unwrap()
                            .to_ascii_lowercase();
                        assert!(!headers.contains("\r\nauthorization:"));
                        assert!(!headers.contains("\r\ncookie:"));
                        assert!(!headers.contains("\r\norigin:"));
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break (end + 4, length);
                        }
                    }
                };
                record
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes[end..end + length]).unwrap());
                if status == 0 {
                    continue;
                }
                if value.get("verification_uri").is_some() {
                    value["verification_uri"] = json!(format!("{origin_copy}/api/auth/device"));
                }
                let body = if status == 204 {
                    Vec::new()
                } else {
                    serde_json::to_vec(&value).unwrap()
                };
                let headers = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(headers.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        (origin, captured, server)
    }
    #[tokio::test]
    async fn login_pending_rotation_and_revoke_follow_production_json_contract() {
        let mut initial = reply("tenant-a");
        initial["expires_in"] = json!(1);
        let mut rotated = reply("tenant-a");
        rotated["refresh_token"] = json!("rotated-refresh");
        let (origin,captured,server)=http_fixture(vec![
            (200,json!({"device_code":"opaque-device-code","user_code":"TEST-CODE","verification_uri":"replaced","expires_in":20,"interval":1})),
            (400,json!({"error":"authorization_pending"})),(200,initial),(200,rotated),(204,Value::Null)]).await;
        let login =
            DeviceLogin::with_vault(&origin, "agentforge-cli", Box::new(Vault::default())).unwrap();
        let verifier = DeviceLogin::verifier();
        let challenge = login.begin(&verifier).await.unwrap();
        login.complete(challenge, &verifier).await.unwrap();
        assert_eq!(login.access_token().await.unwrap().tenant_id, "tenant-a");
        assert_eq!(login.logout().await.unwrap()["revoked"], true);
        server.await.unwrap();
        let requests = captured.lock().unwrap();
        assert_eq!(requests[0]["client_id"], "agentforge-cli");
        assert_eq!(requests[0]["scope"], "platform");
        assert_eq!(
            requests[0]["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
        );
        assert!(requests[0].get("tenant_id").is_none());
        assert_eq!(requests[1]["code_verifier"], verifier);
        assert_eq!(requests[3]["grant_type"], "refresh_token");
        assert_eq!(requests[4], json!({"refresh_token":"rotated-refresh"}));
        assert!(!login.status().unwrap()["authenticated"].as_bool().unwrap());
    }
    #[tokio::test]
    async fn lost_refresh_response_is_durably_non_replayable() {
        let (origin, captured, server) = http_fixture(vec![(0, Value::Null)]).await;
        let login =
            DeviceLogin::with_vault(&origin, "agentforge-cli", Box::new(Vault::default())).unwrap();
        let mut value = reply("tenant-a");
        value["expires_in"] = json!(1);
        login.save_reply(value, None).unwrap();
        assert!(matches!(
            login.access_token().await,
            Err(Error::UnknownOutcome)
        ));
        assert!(login.load().unwrap().unwrap().refresh_in_flight);
        assert!(matches!(
            login.access_token().await,
            Err(Error::Authentication)
        ));
        server.await.unwrap();
        assert_eq!(captured.lock().unwrap().len(), 1);
    }
}
