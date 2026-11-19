use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::Mutex;
use uuid::Uuid;

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const DYNAMIC: &str = "dynamic_agent_client";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

#[derive(Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct SecretString(String);
impl SecretString {
    pub(crate) fn from_callback(value: String) -> Self {
        Self(value)
    }
    /// Explicit credential access for a secure browser handoff/storage boundary only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountIdentity {
    pub client_id: String,
    pub subject: String,
    pub email: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Tokens {
    access_token: SecretString,
    refresh_token: Option<SecretString>,
    id_token: SecretString,
    scope: String,
    expires_at: u64,
}
pub struct Account {
    pub(crate) identity: AccountIdentity,
    pub(crate) tokens: Arc<Mutex<Option<Tokens>>>,
    pub(crate) store: Option<Arc<super::store::ProtectedStore>>,
}
impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Account([REDACTED])")
    }
}
impl Account {
    pub fn identity(&self) -> &AccountIdentity {
        &self.identity
    }
    pub async fn plan_enabled(&self) -> bool {
        self.tokens
            .lock()
            .await
            .as_ref()
            .is_some_and(|t| has_direct_scope(&t.scope))
    }
    /// Save through a protected store and retain it for atomic rotation persistence.
    pub async fn persist(mut self, store: Arc<super::store::ProtectedStore>) -> Result<Self> {
        let shared = store.session(&self.identity, self.tokens.clone())?;
        if !Arc::ptr_eq(&shared, &self.tokens) {
            let mut existing = shared.lock().await;
            *existing = self.tokens.lock().await.clone();
            store.save_record(&self.identity, &existing)?;
        } else {
            store.save_record(&self.identity, &*self.tokens.lock().await)?;
        }
        self.tokens = shared;
        self.store = Some(store);
        Ok(self)
    }
}
fn has_direct_scope(scope: &str) -> bool {
    scope.split_ascii_whitespace().any(|s| s == DIRECT_SCOPE)
}
fn random_value() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct AuthorizationAttempt {
    state: String,
    nonce: String,
    verifier: String,
    redirect_uri: String,
    client_id: String,
    expected_subject: Option<String>,
    created: Instant,
    authorization_url: SecretString,
}
impl fmt::Debug for AuthorizationAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizationAttempt([REDACTED])")
    }
}
impl AuthorizationAttempt {
    /// Call only after binding the listener and after an explicit user sign-in action.
    pub async fn new(
        host_id: &str,
        app_name: &str,
        redirect_uri: &str,
        returning: Option<&Account>,
    ) -> Result<Self> {
        let callback =
            Url::parse(redirect_uri).map_err(|_| PlanError::local("invalid_loopback_callback"))?;
        if callback.scheme() != "http"
            || callback.host_str() != Some("127.0.0.1")
            || callback.port().is_none()
            || callback.path() != "/auth/callback"
            || callback.query().is_some()
            || callback.fragment().is_some()
            || !callback.username().is_empty()
            || callback.password().is_some()
            || !valid_host_id(host_id)
            || app_name.is_empty()
        {
            return Err(PlanError::local("invalid_loopback_callback"));
        }
        let state = random_value();
        let nonce = random_value();
        let verifier = random_value();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let client_id = returning
            .map(|a| a.identity.client_id.as_str())
            .unwrap_or(DYNAMIC)
            .to_owned();
        if returning.is_some() && (client_id == DYNAMIC || client_id.is_empty()) {
            return Err(PlanError::local("invalid_client"));
        }
        let id_hint = if let Some(account) = returning {
            account
                .tokens
                .lock()
                .await
                .as_ref()
                .map(|t| t.id_token.clone())
        } else {
            None
        };
        let mut url = Url::parse(AUTHORIZE).expect("constant URL");
        {
            let mut q = url.query_pairs_mut();
            q.extend_pairs([
                ("client_id", client_id.as_str()),
                ("ext_agent_host_id", host_id),
                ("response_type", "code"),
                ("redirect_uri", redirect_uri),
                ("scope", SCOPES),
                ("resource", RESOURCE),
                ("state", &state),
                ("nonce", &nonce),
                ("code_challenge_method", "S256"),
                ("code_challenge", &challenge),
            ]);
            if let Some(account) = returning {
                if let Some(id) = &id_hint {
                    q.append_pair("id_token_hint", id.expose());
                }
                if let Some(email) = &account.identity.email {
                    q.append_pair("login_hint", email);
                }
            } else {
                q.append_pair("agent_name_hint", app_name);
            }
        }
        Ok(Self {
            state,
            nonce,
            verifier,
            redirect_uri: redirect_uri.into(),
            client_id,
            expected_subject: returning.map(|a| a.identity.subject.clone()),
            created: Instant::now(),
            authorization_url: SecretString(url.into()),
        })
    }
    /// Retain this issued registration before exchange, including if exchange later returns invalid_grant.
    pub fn validated_callback_client_id(&self, callback: &str) -> Result<String> {
        self.callback(callback).map(|(id, _)| id)
    }
    /// Retry an incomplete registration with fresh state/PKCE rather than creating another client.
    /// This hint is not an authenticated account and cannot perform inference.
    pub async fn resume_registration(
        host_id: &str,
        app_name: &str,
        redirect_uri: &str,
        issued_client_id: &str,
    ) -> Result<Self> {
        if issued_client_id.is_empty() || issued_client_id == DYNAMIC {
            return Err(PlanError::local("invalid_client"));
        }
        let mut attempt = Self::new(host_id, app_name, redirect_uri, None).await?;
        let mut url = Url::parse(attempt.authorization_url.expose())
            .map_err(|_| PlanError::local("invalid_authorization_url"))?;
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "client_id" && key != "agent_name_hint")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair("client_id", issued_client_id);
        attempt.client_id = issued_client_id.into();
        attempt.authorization_url = SecretString(url.into());
        Ok(attempt)
    }
    /// Call only when the user explicitly asks to enable plan permission after declining it.
    pub fn request_plan_consent(mut self) -> Self {
        let mut url =
            Url::parse(self.authorization_url.expose()).expect("validated authorization URL");
        url.query_pairs_mut().append_pair("prompt", "consent");
        self.authorization_url = SecretString(url.into());
        self
    }
    /// Headless handoff may display the authorization URL only after removing token/email hints.
    /// Expected returning identity checks remain intact.
    pub fn without_login_hints(mut self) -> Self {
        let mut url =
            Url::parse(self.authorization_url.expose()).expect("validated authorization URL");
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != "id_token_hint" && k != "login_hint")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.set_query(None);
        url.query_pairs_mut().extend_pairs(pairs);
        self.authorization_url = SecretString(url.into());
        self
    }
    pub fn authorization_url(&self) -> &SecretString {
        &self.authorization_url
    }
    fn callback(&self, callback: &str) -> Result<(String, String)> {
        if self.created.elapsed() > Duration::from_secs(600) {
            return Err(PlanError::local("authorization_expired"));
        }
        let url = Url::parse(callback).map_err(|_| PlanError::local("invalid_callback"))?;
        let mut base = url.clone();
        base.set_query(None);
        if base.as_str() != self.redirect_uri {
            return Err(PlanError::local("callback_uri_mismatch"));
        }
        let mut params = HashMap::new();
        for (k, v) in url.query_pairs() {
            if params.insert(k.into_owned(), v.into_owned()).is_some() {
                return Err(PlanError::local("duplicate_callback_parameter"));
            }
        }
        if params.get("state") != Some(&self.state) {
            return Err(PlanError::local("state_mismatch"));
        }
        if params.contains_key("error") {
            return Err(PlanError::local("authorization_declined"));
        }
        let issued = if self.client_id == DYNAMIC {
            params
                .get("client_id")
                .filter(|v| !v.is_empty() && v.as_str() != DYNAMIC)
                .cloned()
                .ok_or_else(|| PlanError::local("registration_incomplete"))?
        } else {
            if params
                .get("client_id")
                .is_some_and(|v| *v != self.client_id)
            {
                return Err(PlanError::local("client_mismatch"));
            }
            self.client_id.clone()
        };
        let code = params
            .get("code")
            .filter(|v| !v.is_empty())
            .cloned()
            .ok_or_else(|| PlanError::local("missing_code"))?;
        Ok((issued, code))
    }
}
#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    scope: Option<String>,
    expires_in: u64,
    token_type: String,
}
#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    revocation_endpoint: Option<String>,
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    email: Option<String>,
    nonce: Option<String>,
}
fn official_auth_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| PlanError::local("invalid_discovery"))?;
    if url.scheme() != "https"
        || url.host_str() != Some("auth.openai.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(PlanError::local("invalid_discovery"));
    }
    Ok(url)
}
impl ChatGptPlanClient {
    async fn discovery(&self) -> Result<Discovery> {
        let response = self
            .http
            .get(DISCOVERY)
            .send()
            .await
            .map_err(|_| PlanError::local("transport"))?;
        let d: Discovery = serde_json::from_value(checked_json(response, &[]).await?)
            .map_err(|_| PlanError::local("invalid_discovery"))?;
        if d.issuer != ISSUER {
            return Err(PlanError::local("issuer_mismatch"));
        }
        official_auth_url(&d.jwks_uri)?;
        Ok(d)
    }
    async fn verify_identity(
        &self,
        token: &str,
        client_id: &str,
        nonce: Option<&str>,
    ) -> Result<Claims> {
        let d = self.discovery().await?;
        let response = self
            .http
            .get(official_auth_url(&d.jwks_uri)?)
            .send()
            .await
            .map_err(|_| PlanError::local("transport"))?;
        let keys: JwkSet = serde_json::from_value(checked_json(response, &[token]).await?)
            .map_err(|_| PlanError::local("invalid_jwks"))?;
        let header = decode_header(token).map_err(|_| PlanError::local("invalid_identity"))?;
        if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
            return Err(PlanError::local("invalid_identity_algorithm"));
        }
        let kid = header
            .kid
            .ok_or_else(|| PlanError::local("missing_key_id"))?;
        let jwk = keys
            .find(&kid)
            .ok_or_else(|| PlanError::local("unknown_signing_key"))?;
        let key =
            DecodingKey::from_jwk(jwk).map_err(|_| PlanError::local("invalid_signing_key"))?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[ISSUER]);
        validation.set_audience(&[client_id]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub", "iat"]);
        validation.leeway = 5;
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|_| PlanError::local("invalid_identity"))?
            .claims;
        if claims.sub.is_empty() || nonce.is_some_and(|n| claims.nonce.as_deref() != Some(n)) {
            return Err(PlanError::local("identity_nonce_mismatch"));
        }
        Ok(claims)
    }
    /// Consumes the attempt, preventing authorization code reuse through this API.
    pub async fn exchange(
        &self,
        attempt: AuthorizationAttempt,
        callback_url: &str,
    ) -> Result<Account> {
        let (client_id, code) = attempt.callback(callback_url)?;
        let response = self
            .http
            .post(TOKEN)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", attempt.verifier.as_str()),
                ("redirect_uri", attempt.redirect_uri.as_str()),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| PlanError::local("transport"))?;
        let reply: TokenReply =
            serde_json::from_value(checked_json(response, &[&code, &attempt.verifier]).await?)
                .map_err(|_| PlanError::local("invalid_token_reply"))?;
        if !reply.token_type.eq_ignore_ascii_case("bearer") || reply.access_token.is_empty() {
            return Err(PlanError::local("invalid_token_reply"));
        }
        let id_token = reply
            .id_token
            .ok_or_else(|| PlanError::local("missing_id_token"))?;
        let claims = self
            .verify_identity(&id_token, &client_id, Some(&attempt.nonce))
            .await?;
        if attempt.expected_subject.is_some_and(|s| s != claims.sub) {
            return Err(PlanError::local("account_mismatch"));
        }
        Ok(Account {
            identity: AccountIdentity {
                client_id,
                subject: claims.sub,
                email: claims.email,
            },
            tokens: Arc::new(Mutex::new(Some(Tokens {
                access_token: SecretString(reply.access_token),
                refresh_token: reply.refresh_token.map(SecretString),
                id_token: SecretString(id_token),
                scope: reply.scope.unwrap_or_default(),
                expires_at: now().saturating_add(reply.expires_in),
            }))),
            store: None,
        })
    }
    pub(crate) async fn access_token(&self, account: &Account) -> Result<SecretString> {
        // Held across refresh and durable save: one session cannot race rotating tokens.
        let mut guard = account.tokens.lock().await;
        let t = guard
            .as_ref()
            .ok_or_else(|| PlanError::local("sign_in_required"))?;
        if !has_direct_scope(&t.scope) {
            return Err(PlanError::local("plan_permission_disabled"));
        }
        if t.expires_at > now().saturating_add(60) {
            return Ok(t.access_token.clone());
        }
        let refresh = t
            .refresh_token
            .as_ref()
            .ok_or_else(|| PlanError::local("sign_in_required"))?;
        let response = self
            .http
            .post(TOKEN)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", account.identity.client_id.as_str()),
                ("refresh_token", refresh.expose()),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| PlanError::local("transport"))?;
        let value = match checked_json(
            response,
            &[
                refresh.expose(),
                t.access_token.expose(),
                t.id_token.expose(),
            ],
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                if e.requires_sign_in() {
                    *guard = None;
                    if let Some(store) = &account.store {
                        store.save_record(&account.identity, &guard)?;
                    }
                }
                return Err(e);
            }
        };
        let reply: TokenReply =
            serde_json::from_value(value).map_err(|_| PlanError::local("invalid_token_reply"))?;
        if !reply.token_type.eq_ignore_ascii_case("bearer") || reply.access_token.is_empty() {
            return Err(PlanError::local("invalid_token_reply"));
        }
        if let Some(id) = &reply.id_token {
            let identity = self
                .verify_identity(id, &account.identity.client_id, None)
                .await?;
            if identity.sub != account.identity.subject {
                return Err(PlanError::local("account_mismatch"));
            }
        }
        let t = guard.as_mut().expect("token retained");
        t.access_token = SecretString(reply.access_token);
        if let Some(refresh) = reply.refresh_token {
            t.refresh_token = Some(SecretString(refresh));
        }
        if let Some(id) = reply.id_token {
            t.id_token = SecretString(id);
        }
        if let Some(scope) = reply.scope {
            t.scope = scope;
        }
        t.expires_at = now().saturating_add(reply.expires_in);
        if let Some(store) = &account.store {
            store.save_record(&account.identity, &guard)?;
        }
        let t = guard.as_ref().expect("token retained");
        if !has_direct_scope(&t.scope) {
            return Err(PlanError::local("plan_permission_disabled"));
        }
        Ok(t.access_token.clone())
    }
    /// Stops local requests even when remote revocation fails; error means unconfirmed remote sign-out.
    pub async fn sign_out(&self, account: &Account) -> Result<()> {
        let mut guard = account.tokens.lock().await;
        let result = async {
            if let Some(refresh) = guard.as_ref().and_then(|t| t.refresh_token.as_ref()) {
                let d = self.discovery().await?;
                let endpoint = d
                    .revocation_endpoint
                    .ok_or_else(|| PlanError::local("missing_revocation_endpoint"))?;
                let response = self
                    .http
                    .post(official_auth_url(&endpoint)?)
                    .form(&[
                        ("token", refresh.expose()),
                        ("token_type_hint", "refresh_token"),
                        ("client_id", account.identity.client_id.as_str()),
                    ])
                    .send()
                    .await
                    .map_err(|_| PlanError::local("revocation_unconfirmed"))?;
                if response.status().as_u16() != 200 {
                    return Err(upstream_error(response, &[refresh.expose()]).await);
                }
            }
            Ok(())
        }
        .await;
        *guard = None;
        if let Some(store) = &account.store {
            store.save_record(&account.identity, &guard)?;
        }
        result
    }
}

pub(crate) fn valid_host_id(value: &str) -> bool {
    if let Some(id) = value.strip_prefix("urn:uuid:") {
        return Uuid::parse_str(id)
            .is_ok_and(|uuid| uuid.get_version_num() == 4 && uuid.hyphenated().to_string() == id);
    }
    value
        .strip_prefix("urn:ietf:params:oauth:jwk-thumbprint:")
        .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
        || value
            .strip_prefix("did:key:")
            .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
}
