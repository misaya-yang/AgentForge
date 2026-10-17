//! Shared HMAC v1/v2 gateway boundary and trusted identity extraction.
//! Startup supplies keys/replay backend; this module never reads environment secrets.
use super::knowledge_types::{QueryError, Result, UserContext};
use crate::actor_verifier::{
    JwtActorVerifier, SelectedCredential, select_credential,
};
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderName, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
#[derive(Clone)]
pub struct VerifiedGateway {
    // Only this module can mint a proof. Keep the exact configured authority
    // alive so pointer identity cannot be reused by a different auth instance.
    authority: Arc<GatewayAuth>,
    request_digest: [u8; 32],
}
impl VerifiedGateway {
    fn new(authority: Arc<GatewayAuth>, parts: &axum::http::request::Parts, body: &[u8]) -> Self {
        Self {
            authority,
            request_digest: request_digest(parts, body),
        }
    }
    fn matches(
        &self,
        authority: &Arc<GatewayAuth>,
        parts: &axum::http::request::Parts,
        body: &[u8],
    ) -> bool {
        Arc::ptr_eq(&self.authority, authority)
            && self.request_digest == request_digest(parts, body)
    }
}
fn request_digest(parts: &axum::http::request::Parts, body: &[u8]) -> [u8; 32] {
    fn field(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }
    let mut hash = Sha256::new();
    field(&mut hash, parts.method.as_str().as_bytes());
    field(&mut hash, parts.uri.to_string().as_bytes());
    field(&mut hash, body);
    for name in [
        "x-gateway-secret",
        "x-user-id",
        "x-tenant-id",
        "x-user-tier",
        "x-user-type",
        "x-user-roles",
        "x-user-email",
        "x-user-name",
        "x-app-user-id",
        "x-app-tenant-id",
    ] {
        field(&mut hash, name.as_bytes());
        let values = parts.headers.get_all(name);
        hash.update((values.iter().count() as u64).to_be_bytes());
        for value in values.iter() {
            field(&mut hash, value.as_bytes());
        }
    }
    hash.finalize().into()
}
#[derive(Clone)]
pub enum ReplayStore {
    Memory(Arc<Mutex<BTreeMap<String, u128>>>),
    Redis(redis::aio::ConnectionManager),
}
impl ReplayStore {
    pub fn memory() -> Self {
        Self::Memory(Arc::new(Mutex::new(BTreeMap::new())))
    }
    async fn seen_or_record(&self, id: &str, ttl: u64) -> Result<bool> {
        match self {
            Self::Memory(store) => {
                let now = epoch_ms()?;
                let mut store = store.lock().await;
                store.retain(|_, expires| *expires > now);
                if store.contains_key(id) {
                    Ok(true)
                } else {
                    store.insert(id.into(), now + ttl as u128);
                    Ok(false)
                }
            }
            Self::Redis(connection) => {
                let mut connection = connection.clone();
                let recorded: Option<String> = redis::cmd("SET")
                    .arg(format!("ai-gateway:internal:replay:{id}"))
                    .arg("1")
                    .arg("NX")
                    .arg("PX")
                    .arg(ttl)
                    .query_async(&mut connection)
                    .await
                    .map_err(|_| QueryError::Forbidden)?;
                Ok(recorded.is_none())
            }
        }
    }
}
#[derive(Clone)]
pub struct GatewayAuth {
    secret: Arc<String>,
    keys: Arc<BTreeMap<String, String>>,
    pub version: String,
    pub caller_service: String,
    pub audience: String,
    pub allowed_path_prefixes: Vec<String>,
    pub replay: ReplayStore,
    pub max_skew_ms: u64,
    pub replay_ttl_ms: u64,
    pub allow_anonymous: bool,
    pub separately_authenticated_paths: Vec<String>,
    pub separately_authenticated_prefixes: Vec<String>,
}
impl GatewayAuth {
    pub fn new(
        secret: String,
        keys: BTreeMap<String, String>,
        version: String,
        replay: ReplayStore,
    ) -> Result<Self> {
        if secret.chars().count() < 16
            || !["v1", "v2"].contains(&version.as_str())
            || keys
                .iter()
                .any(|(k, v)| k.is_empty() || k.contains(':') || v.chars().count() < 16)
        {
            return Err(QueryError::Validation(
                "invalid internal authentication configuration".into(),
            ));
        }
        let keys = if keys.is_empty() {
            BTreeMap::from([("local".into(), secret.clone())])
        } else {
            keys
        };
        Ok(Self {
            secret: Arc::new(secret),
            keys: Arc::new(keys),
            version,
            caller_service: "gateway".into(),
            audience: "knowledge-service".into(),
            allowed_path_prefixes: vec!["/api/v1".into()],
            replay,
            max_skew_ms: 60_000,
            replay_ttl_ms: 120_000,
            allow_anonymous: false,
            separately_authenticated_paths: vec![],
            separately_authenticated_prefixes: vec![],
        })
    }
    pub fn for_knowledge_service(
        secret: String,
        keys: BTreeMap<String, String>,
        environment: &str,
        replay: ReplayStore,
    ) -> Result<Self> {
        let local = ["local", "dev", "development", "test", "testing"]
            .contains(&environment.trim().to_lowercase().as_str());
        if !local && matches!(replay, ReplayStore::Memory(_)) {
            return Err(QueryError::Validation(
                "internal replay protection must use Redis outside local development/test".into(),
            ));
        }
        let mut config = Self::new(secret, keys, "v2".into(), replay)?;
        config.separately_authenticated_paths = vec!["/version".into()];
        config.separately_authenticated_prefixes = vec!["/internal/v2/capabilities/".into()];
        Ok(config)
    }
    pub fn validate_service_boundary(&self) -> Result<()> {
        if (self.allow_anonymous && !self.secret.is_empty())
            || (!self.allow_anonymous && (self.secret.is_empty() || self.version != "v2"))
        {
            return Err(QueryError::Validation("knowledge service requires signed v2 traffic or explicit anonymous development mode".into()));
        }
        Ok(())
    }
    /// Source permits anonymous operation only when the shared secret is unset
    /// and the operator explicitly selected a local/test environment.
    pub fn anonymous_development(environment: &str) -> Result<Self> {
        if !["local", "dev", "development", "test", "testing"]
            .contains(&environment.trim().to_lowercase().as_str())
        {
            return Err(QueryError::Validation(
                "anonymous knowledge service is restricted to local development/test".into(),
            ));
        }
        Ok(Self {
            secret: Arc::new(String::new()),
            keys: Arc::new(BTreeMap::new()),
            version: "v2".into(),
            caller_service: "gateway".into(),
            audience: "knowledge-service".into(),
            allowed_path_prefixes: vec!["/api/v1".into()],
            replay: ReplayStore::memory(),
            max_skew_ms: 60_000,
            replay_ttl_ms: 120_000,
            allow_anonymous: true,
            separately_authenticated_paths: vec![],
            separately_authenticated_prefixes: vec![],
        })
    }
    pub async fn verify(
        &self,
        header: &str,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
        headers: &HeaderMap,
    ) -> Result<String> {
        let parts: Vec<_> = header.split(':').collect();
        if parts.first().copied() != Some(self.version.as_str()) {
            return Err(QueryError::Forbidden);
        }
        let (rid, timestamp, signature, secret, canonical) = if self.version == "v1" {
            if parts.len() != 4 {
                return Err(QueryError::Forbidden);
            }
            (
                parts[1],
                parts[2],
                parts[3],
                self.secret.as_str(),
                format!("{}:{}", parts[1], parts[2]),
            )
        } else {
            if parts.len() != 6
                || (!self.allowed_path_prefixes.is_empty()
                    && !self.allowed_path_prefixes.iter().any(|p| {
                        path == p
                            || (p == "/" && path.starts_with('/'))
                            || path.starts_with(&format!("{}/", p.trim_end_matches('/')))
                    }))
            {
                return Err(QueryError::Forbidden);
            }
            let secret = self.keys.get(parts[1]).ok_or(QueryError::Forbidden)?;
            let body_hash = format!("{:x}", Sha256::digest(body));
            if body_hash != parts[4] {
                return Err(QueryError::Forbidden);
            }
            let identity = canonical_identity_headers(headers)?;
            (
                parts[2],
                parts[3],
                parts[5],
                secret.as_str(),
                [
                    self.caller_service.as_str(),
                    self.audience.as_str(),
                    &method.to_uppercase(),
                    path,
                    query,
                    parts[4],
                    &identity,
                    parts[2],
                    parts[3],
                    parts[1],
                ]
                .join("\n"),
            )
        };
        let ts = timestamp
            .parse::<i128>()
            .map_err(|_| QueryError::Forbidden)?;
        let now = epoch_ms()? as i128;
        if (now - ts).unsigned_abs() > self.max_skew_ms as u128 {
            return Err(QueryError::Forbidden);
        }
        let bytes = hex_signature(signature)?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| QueryError::Forbidden)?;
        mac.update(canonical.as_bytes());
        mac.verify_slice(&bytes)
            .map_err(|_| QueryError::Forbidden)?;
        if self.replay.seen_or_record(rid, self.replay_ttl_ms).await? {
            return Err(QueryError::Forbidden);
        }
        Ok(rid.into())
    }
}
fn epoch_ms() -> Result<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .map_err(|_| QueryError::Forbidden)
}
fn hex_signature(s: &str) -> Result<Vec<u8>> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(QueryError::Forbidden);
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| QueryError::Forbidden))
        .collect()
}
fn canonical_identity_headers(headers: &HeaderMap) -> Result<String> {
    let mut values = BTreeMap::new();
    for name in [
        "x-user-id",
        "x-tenant-id",
        "x-user-tier",
        "x-user-type",
        "x-user-roles",
        "x-user-email",
        "x-user-name",
        "x-app-user-id",
        "x-app-tenant-id",
    ] {
        if let Some(value) = headers.get(name) {
            values.insert(name, value.to_str().map_err(|_| QueryError::Forbidden)?);
        }
    }
    Ok(values
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join("\n"))
}
fn trusted_identity(headers: &HeaderMap, anonymous: bool) -> Result<UserContext> {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let user_id = get("x-user-id");
    let tenant_id = get("x-tenant-id");
    if user_id.is_empty() || tenant_id.is_empty() {
        return if anonymous {
            Ok(UserContext {
                user_id: "anonymous".into(),
                tenant_id: "default".into(),
                user_tier: "anonymous".into(),
                user_type: "anonymous".into(),
                roles: vec!["guest".into()],
                is_authenticated: false,
            })
        } else {
            Err(QueryError::Forbidden)
        };
    }
    let mut tier = get("x-user-tier");
    if tier.is_empty() {
        tier = "normal".into()
    }
    let mut user_type = get("x-user-type");
    if user_type.is_empty() {
        user_type = "user".into()
    }
    let roles = get("x-user-roles");
    let roles = if roles.is_empty() {
        vec!["user".into()]
    } else {
        roles
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let mut context = UserContext {
        user_id,
        tenant_id,
        user_tier: tier,
        user_type,
        roles,
        is_authenticated: true,
    };
    if context.anonymous() {
        context.is_authenticated = false
    }
    Ok(context)
}
fn denied() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"code":"AUTH_DENIED","message":"Missing or invalid X-Gateway-Secret"})),
    )
        .into_response()
}
pub async fn verify_gateway(
    State(auth): State<Arc<GatewayAuth>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if ["/health", "/health/live", "/health/ready", "/metrics"].contains(&path.as_str())
        || auth.separately_authenticated_paths.contains(&path)
        || auth
            .separately_authenticated_prefixes
            .iter()
            .any(|p| path.starts_with(p))
    {
        return next.run(request).await;
    }
    let header = request
        .headers()
        .get("x-gateway-secret")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if header.is_none() && !auth.allow_anonymous {
        return denied();
    }
    let signed = header.is_some();
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => return denied(),
    };
    let already_verified = parts
        .extensions
        .get::<VerifiedGateway>()
        .is_some_and(|proof| proof.matches(&auth, &parts, &bytes));
    if let Some(header) = header {
        if !already_verified
            && auth
                .verify(
                    &header,
                    parts.method.as_str(),
                    &path,
                    parts.uri.query().unwrap_or(""),
                    &bytes,
                    &parts.headers,
                )
                .await
                .is_err()
        {
            return denied();
        }
    }
    let identity = match trusted_identity(&parts.headers, auth.allow_anonymous) {
        Ok(identity) => identity,
        Err(_) => return denied(),
    };
    let proof = signed.then(|| VerifiedGateway::new(auth.clone(), &parts, &bytes));
    let mut request = Request::from_parts(parts, Body::from(bytes));
    request.extensions_mut().insert(identity);
    if let Some(proof) = proof {
        request.extensions_mut().insert(proof);
    }
    next.run(request).await
}
pub async fn verified_actor<V: JwtActorVerifier + ?Sized>(auth: &V, headers: &HeaderMap) -> Result<UserContext> {
    let token = match select_credential(headers, &HeaderName::from_static("x-api-key")) {
        Ok(SelectedCredential::Jwt(t)) => t,
        _ => return Err(QueryError::Forbidden),
    };
    let p = auth
        .verify_jwt_token(token)
        .await
        .map_err(|_| QueryError::Forbidden)?;
    Ok(UserContext {
        user_id: p.user_id,
        tenant_id: p.tenant_id,
        user_tier: p.tier,
        roles: p.roles,
        user_type: "user".into(),
        is_authenticated: true,
    })
}
pub async fn verify_jwt<V: JwtActorVerifier + ?Sized + 'static>(
    State(auth): State<Arc<V>>,
    mut request: Request,
    next: Next,
) -> Response {
    match verified_actor(&auth, request.headers()).await {
        Ok(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(_) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"detail":"Authentication required"})),
        )
            .into_response(),
    }
}
impl GatewayAuth {
    /// Signs the actual serialized bytes; callers cannot reuse a signature with a
    /// different method, target, identity set, query string or request body.
    pub fn sign(
        &self,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
        headers: &HeaderMap,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = epoch_ms()?.to_string();
        if self.version == "v1" {
            let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
                .map_err(|_| QueryError::Forbidden)?;
            mac.update(format!("{id}:{timestamp}").as_bytes());
            return Ok(format!(
                "v1:{id}:{timestamp}:{}",
                mac.finalize()
                    .into_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ));
        }
        let (key, secret) = self.keys.iter().next().ok_or(QueryError::Forbidden)?;
        let hash = format!("{:x}", Sha256::digest(body));
        let identity = canonical_identity_headers(headers)?;
        let canonical = [
            self.caller_service.as_str(),
            self.audience.as_str(),
            &method.to_uppercase(),
            path,
            query,
            &hash,
            &identity,
            &id,
            &timestamp,
            key,
        ]
        .join("\n");
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| QueryError::Forbidden)?;
        mac.update(canonical.as_bytes());
        Ok(format!(
            "v2:{key}:{id}:{timestamp}:{hash}:{}",
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
    }
}
/// Signed service-to-service routes such as RAGAS carry no end-user identity.
/// The signature and replay proof remain mandatory even in anonymous dev mode.
pub async fn verify_gateway_signature(
    State(auth): State<Arc<GatewayAuth>>,
    request: Request,
    next: Next,
) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => return denied(),
    };
    let signature = parts
        .headers
        .get("x-gateway-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let already_verified = parts
        .extensions
        .get::<VerifiedGateway>()
        .is_some_and(|proof| proof.matches(&auth, &parts, &bytes));
    if !already_verified
        && auth
            .verify(
                signature,
                parts.method.as_str(),
                parts.uri.path(),
                parts.uri.query().unwrap_or(""),
                &bytes,
                &parts.headers,
            )
            .await
            .is_err()
    {
        return denied();
    }
    let proof = VerifiedGateway::new(auth, &parts, &bytes);
    let mut request = Request::from_parts(parts, Body::from(bytes));
    request.extensions_mut().insert(proof);
    next.run(request).await
}

#[cfg(test)]
mod actor_verifier_port_tests {
    use super::*;
    use crate::actor_verifier::{
        ActorVerificationError, ActorVerificationFuture, VerifiedActor,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Verifier {
        calls: AtomicUsize,
    }
    impl JwtActorVerifier for Verifier {
        fn verify_jwt_token<'a>(&'a self, token: &'a str) -> ActorVerificationFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if token != "valid-jwt" {
                    return Err(ActorVerificationError);
                }
                Ok(VerifiedActor {
                    user_id: "verified-user".into(),
                    tenant_id: "verified-tenant".into(),
                    tier: "normal".into(),
                    roles: vec!["viewer".into()],
                })
            })
        }
    }
    fn verifier() -> Verifier {
        Verifier {
            calls: AtomicUsize::new(0),
        }
    }
    #[tokio::test]
    async fn actor_identity_comes_only_from_the_explicit_verifier_not_unsigned_headers() {
        let verifier = verifier();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer valid-jwt".parse().unwrap());
        headers.insert("x-user-id", "attacker".parse().unwrap());
        headers.insert("x-tenant-id", "other-tenant".parse().unwrap());
        headers.insert("x-user-tier", "admin".parse().unwrap());
        let actor = verified_actor(&verifier, &headers).await.unwrap();
        assert_eq!(actor.user_id, "verified-user");
        assert_eq!(actor.tenant_id, "verified-tenant");
        assert_eq!(actor.user_tier, "normal");
        assert_eq!(actor.roles, vec!["viewer"]);
        assert!(actor.is_authenticated);
        assert_eq!(actor.user_type, "user");
        assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn malformed_duplicate_missing_or_api_key_credentials_never_reach_jwt_verifier() {
        let verifier = verifier();
        for kind in [
            "missing",
            "api-key",
            "malformed-bearer",
            "extra-token",
            "duplicate-auth",
            "duplicate-key",
            "basic-with-key",
        ] {
            let mut headers = HeaderMap::new();
            match kind {
                "api-key" => {
                    headers.insert("x-api-key", "secret-key".parse().unwrap());
                }
                "malformed-bearer" => {
                    headers.insert("authorization", "Bearer".parse().unwrap());
                }
                "extra-token" => {
                    headers.insert("authorization", "Bearer valid-jwt extra".parse().unwrap());
                }
                "duplicate-auth" => {
                    headers.append("authorization", "Bearer valid-jwt".parse().unwrap());
                    headers.append("authorization", "Bearer other".parse().unwrap());
                }
                "duplicate-key" => {
                    headers.insert("authorization", "Bearer valid-jwt".parse().unwrap());
                    headers.append("x-api-key", "a".parse().unwrap());
                    headers.append("x-api-key", "b".parse().unwrap());
                }
                "basic-with-key" => {
                    headers.insert("authorization", "Basic something".parse().unwrap());
                    headers.insert("x-api-key", "secret-key".parse().unwrap());
                }
                _ => {}
            }
            assert!(
                matches!(
                    verified_actor(&verifier, &headers).await,
                    Err(QueryError::Forbidden)
                ),
                "{kind}"
            );
        }
        assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn failed_bearer_does_not_fall_back_to_accompanying_api_key() {
        let verifier = Arc::new(verifier());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer invalid-jwt".parse().unwrap());
        headers.insert("x-api-key", "valid-looking-key".parse().unwrap());
        assert!(matches!(
            verified_actor(&verifier, &headers).await,
            Err(QueryError::Forbidden)
        ));
        assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
    }
}
