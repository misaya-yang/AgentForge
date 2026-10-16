//! Gateway-owned knowledge reads. Dataset ACL and ownership remain authoritative in KS.
//!
//! No environment, default endpoint, unsigned mode, public identity headers, or database
//! access is used here. The production HTTP mount shares native user-context authority.
use ai_platform_auth::{
    api_keys::ApiKeyAuthority,
    service::{AuthService, Principal},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use hmac::{Hmac, Mac};
use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::IpAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

pub const AUTHORIZE_DATASETS_PATH: &str = "/api/v1/internal/knowledge/datasets/authorize";
pub const DATASET_DETAIL_PATH: &str = "/api/v1/knowledge/datasets";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_OPERATION_BYTES: usize = 16 * 1024 * 1024;
const MAX_CATALOG_PAGES: usize = 100;
const MAX_DATASETS: usize = 20_000;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15);
const MULTIMODAL_MODELS: &[&str] = &[
    "multimodal-embedding-v1",
    "multimodal-embedding-one-peace-v1",
    "multimodal-embedding-one-peace",
    "tongyi-embedding-vision-plus",
    "qwen2.5-vl-embedding",
];

/// Deliberately contains no upstream body, URL, identity, token, or transport error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeError {
    Configuration,
    Unavailable,
    InvalidResponse,
    Scope,
    UpstreamStatus(u16),
}
impl fmt::Display for KnowledgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Knowledge service is unavailable")
    }
}
impl std::error::Error for KnowledgeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentKnowledgeAuthorizationError;
impl AgentKnowledgeAuthorizationError {
    pub const CODE: &'static str = "AGENT_KNOWLEDGE_UNAVAILABLE";
}
impl fmt::Display for AgentKnowledgeAuthorizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::CODE)
    }
}
impl std::error::Error for AgentKnowledgeAuthorizationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetImpactAuthorityError {
    pub status_code: u16,
}
impl fmt::Display for DatasetImpactAuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DATASET_IMPACT_AUTHORITY_UNAVAILABLE")
    }
}
impl std::error::Error for DatasetImpactAuthorityError {}

/// Trusted startup configuration only; never deserialize this from a public request.
/// Secret-bearing types deliberately do not implement Debug or Serialize.
pub struct KnowledgeConfig {
    origin: Url,
    key_id: String,
    secret: String,
}
impl KnowledgeConfig {
    /// HTTPS, or explicit loopback HTTP for isolated tests/development.
    /// Existing container-network HTTP must be explicitly selected by the operator.
    pub fn new(origin: &str, key_id: String, secret: String) -> Result<Self, KnowledgeError> {
        Self::parse(origin, key_id, secret, false)
    }

    /// Explicit opt-in for an operator-controlled, private plaintext KS network.
    /// This does not discover or configure credentials, endpoints, or network access.
    pub fn for_private_http(
        origin: &str,
        key_id: String,
        secret: String,
    ) -> Result<Self, KnowledgeError> {
        Self::parse(origin, key_id, secret, true)
    }

    fn parse(
        origin: &str,
        key_id: String,
        secret: String,
        private_http: bool,
    ) -> Result<Self, KnowledgeError> {
        let origin = Url::parse(origin).map_err(|_| KnowledgeError::Configuration)?;
        let loopback = origin
            .host_str()
            .and_then(|s| s.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
            || !(origin.scheme() == "https"
                || (origin.scheme() == "http" && (loopback || private_http)))
            || key_id.is_empty()
            || key_id.len() > 64
            || !key_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || secret.trim().chars().count() < 16
        {
            return Err(KnowledgeError::Configuration);
        }
        Ok(Self {
            origin,
            key_id,
            secret,
        })
    }
}

/// Identity can only be made from already-verified authority or an explicitly
/// anonymous reader. There is no constructor accepting public identity headers.
#[derive(Clone)]
pub struct KnowledgeActor {
    user_id: String,
    tenant_id: String,
    roles: Vec<String>,
    tier: String,
    authenticated: bool,
}
impl KnowledgeActor {
    pub fn from_principal(principal: &Principal) -> Result<Self, KnowledgeError> {
        let roles: BTreeSet<String> = principal
            .authority
            .roles
            .iter()
            .chain(&principal.authority.permissions)
            .cloned()
            .collect();
        let tier = if roles.contains("admin") {
            "admin"
        } else if roles.contains("enterprise") {
            "enterprise"
        } else if roles.contains("premium") || roles.contains("vip") {
            "premium"
        } else {
            &principal.authority.tier
        };
        Self::checked(Self {
            user_id: principal.account.user_id.clone(),
            tenant_id: principal.account.tenant_id.clone(),
            roles: roles.into_iter().collect(),
            tier: tier.to_owned(),
            authenticated: true,
        })
    }

    /// Accept only the native user-context resolver's result, never request claims.
    pub(crate) fn from_identity(
        user: &crate::migration_content::ContentIdentity,
    ) -> Result<Self, KnowledgeError> {
        if !user.is_authenticated {
            return Self::anonymous(user.tenant_id.clone(), user.user_id.clone());
        }
        let roles: BTreeSet<String> = user
            .roles
            .iter()
            .chain(&user.permissions)
            .cloned()
            .collect();
        let tier = if roles.contains("admin") {
            "admin"
        } else if roles.contains("enterprise") {
            "enterprise"
        } else if roles.contains("premium") || roles.contains("vip") {
            "premium"
        } else {
            &user.tier
        };
        Self::checked(Self {
            user_id: user.user_id.clone(),
            tenant_id: user.tenant_id.clone(),
            roles: roles.into_iter().collect(),
            tier: tier.to_owned(),
            authenticated: true,
        })
    }

    /// Does not authenticate or record key usage. The caller must already have
    /// completed those steps. Owner grants never substitute for declared key scope.
    pub fn from_api_key(key: &ApiKeyAuthority) -> Result<Self, KnowledgeError> {
        if !key.allows_knowledge(false) {
            return Err(KnowledgeError::Scope);
        }
        Self::checked(Self {
            user_id: key.user_id.clone(),
            tenant_id: key.tenant_id.clone(),
            roles: key.roles.clone(),
            tier: key.tier.clone(),
            authenticated: true,
        })
    }

    /// Tenant and reader ID must come from the resolved Agent/share, not headers.
    /// No role, tier, or admin flag can be supplied by an anonymous reader.
    pub fn anonymous(tenant_id: String, user_id: String) -> Result<Self, KnowledgeError> {
        Self::checked(Self {
            user_id,
            tenant_id,
            roles: vec!["guest".into()],
            tier: "anonymous".into(),
            authenticated: false,
        })
    }

    fn checked(mut actor: Self) -> Result<Self, KnowledgeError> {
        if actor.user_id.is_empty() || actor.tenant_id.is_empty() {
            return Err(KnowledgeError::Unavailable);
        }
        // Preserve KBProxyClient's conservative downgrade of guest-like authority.
        if !actor.authenticated
            || actor.user_id.eq_ignore_ascii_case("anonymous")
            || actor.tier.eq_ignore_ascii_case("anonymous")
            || actor.roles.iter().any(|r| r.eq_ignore_ascii_case("guest"))
        {
            actor.authenticated = false;
            actor.tier = "anonymous".into();
            actor.roles = vec!["guest".into()];
        }
        actor.headers(IdentityUse::Catalog)?;
        Ok(actor)
    }

    fn is_tenant_admin(&self) -> bool {
        self.authenticated
            && (self.tier.eq_ignore_ascii_case("admin")
                || self.roles.iter().any(|r| {
                    r.eq_ignore_ascii_case("admin") || r.eq_ignore_ascii_case("tenant_admin")
                }))
    }

    fn headers(&self, purpose: IdentityUse) -> Result<HeaderMap, KnowledgeError> {
        let mut headers = HeaderMap::new();
        for (key, value) in [
            ("x-tenant-id", self.tenant_id.as_str()),
            ("x-user-id", self.user_id.as_str()),
        ] {
            insert_header(&mut headers, key, value)?;
        }
        let roles = self.roles.join(",");
        if !roles.is_empty() || matches!(purpose, IdentityUse::Catalog) {
            insert_header(&mut headers, "x-user-roles", &roles)?;
        }
        if !matches!(purpose, IdentityUse::Owner) {
            insert_header(
                &mut headers,
                "x-user-type",
                if self.authenticated {
                    "user"
                } else {
                    "anonymous"
                },
            )?;
        }
        let tier = match purpose {
            IdentityUse::Catalog => Some(self.tier.as_str()),
            _ if !self.authenticated => Some("anonymous"),
            _ if self.is_tenant_admin() => Some("admin"),
            _ => None,
        };
        if let Some(tier) = tier {
            insert_header(&mut headers, "x-user-tier", tier)?;
        }
        Ok(headers)
    }
}
#[derive(Clone, Copy)]
enum IdentityUse {
    Catalog,
    Authorization,
    Owner,
}
fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), KnowledgeError> {
    if value.len() > 8192 || value.contains(['\r', '\n']) {
        return Err(KnowledgeError::Unavailable);
    }
    let value = HeaderValue::from_str(value).map_err(|_| KnowledgeError::Unavailable)?;
    value.to_str().map_err(|_| KnowledgeError::Unavailable)?;
    headers.insert(HeaderName::from_static(name), value);
    Ok(())
}

/// Reused connections, bounded in-flight calls, no proxy environment, no redirects.
pub struct KnowledgeService {
    config: KnowledgeConfig,
    http: reqwest::Client,
    requests: Semaphore,
}
/// Narrow read-proxy envelope. Only explicitly needed metadata can cross the hop.
pub(crate) struct KnowledgeReadResponse {
    pub status: StatusCode,
    pub payload: Value,
    pub total_count: Option<HeaderValue>,
    pub retry_after: Option<HeaderValue>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentSourceReference {
    pub document_id: String,
    pub source_version: i64,
    pub source_hash: String,
}
impl KnowledgeService {
    pub fn new(config: KnowledgeConfig) -> Result<Self, KnowledgeError> {
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|_| KnowledgeError::Configuration)?;
        Ok(Self {
            config,
            http,
            requests: Semaphore::new(32),
        })
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url, KnowledgeError> {
        if segments.iter().any(|s| {
            s.is_empty()
                || *s == "."
                || *s == ".."
                || s.len() > 1024
                || s.contains(['/', '\\', '\r', '\n', '?', '#', '%'])
        }) {
            return Err(KnowledgeError::Unavailable);
        }
        let mut url = self.config.origin.clone();
        url.path_segments_mut()
            .map_err(|_| KnowledgeError::Configuration)?
            .clear()
            .extend(segments);
        Ok(url)
    }

    fn signed_request(
        &self,
        method: Method,
        url: Url,
        signed_path: &str,
        mut headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<reqwest::Request, KnowledgeError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| KnowledgeError::Unavailable)?
            .as_millis()
            .to_string();
        let request_id = uuid::Uuid::new_v4().simple().to_string();
        let signature = signature(
            &self.config,
            &method,
            signed_path,
            url.query().unwrap_or(""),
            &headers,
            &body,
            &request_id,
            &timestamp,
        )?;
        insert_header(&mut headers, "x-gateway-secret", &signature)?;
        headers
            .get_mut("x-gateway-secret")
            .ok_or(KnowledgeError::Unavailable)?
            .set_sensitive(true);
        insert_header(&mut headers, "x-request-id", &request_id)?;
        insert_header(&mut headers, "content-type", "application/json")?;
        self.http
            .request(method, url)
            .headers(headers)
            .body(body)
            .build()
            .map_err(|_| KnowledgeError::Unavailable)
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        signed_path: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<(Value, Option<String>, usize), KnowledgeError> {
        let _permit = tokio::time::timeout(Duration::from_secs(2), self.requests.acquire())
            .await
            .map_err(|_| KnowledgeError::Unavailable)?
            .map_err(|_| KnowledgeError::Unavailable)?;
        let request = self.signed_request(method, url, signed_path, headers, body)?;
        let mut response = self
            .http
            .execute(request)
            .await
            .map_err(|_| KnowledgeError::Unavailable)?;
        if !response.status().is_success() {
            return Err(KnowledgeError::UpstreamStatus(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(KnowledgeError::InvalidResponse);
        }
        let cursor = response
            .headers()
            .get("x-next-cursor")
            .map(|v| v.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| KnowledgeError::InvalidResponse)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| KnowledgeError::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(KnowledgeError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| KnowledgeError::InvalidResponse)?;
        Ok((value, cursor, bytes.len()))
    }

    /// Only the allowlisted document GET routes may use this response-preserving
    /// adapter. URL authority, actor headers, HTTP method, and body are never public inputs.
    pub(crate) async fn document_read_response(
        &self,
        actor: &KnowledgeActor,
        segments: &[&str],
        query: &[(&str, String)],
    ) -> Result<KnowledgeReadResponse, KnowledgeError> {
        let valid_route = match segments {
            ["api", "v1", "knowledge", _, "documents"]
            | ["api", "v1", "knowledge", _, "documents", _]
            | ["api", "v1", "knowledge", _, "documents", _, "source"]
            | ["api", "v1", "knowledge", _, "documents", _, "versions"] => true,
            [
                "api",
                "v1",
                "knowledge",
                _,
                "documents",
                _,
                "versions",
                version,
            ]
            | [
                "api",
                "v1",
                "knowledge",
                _,
                "documents",
                _,
                "versions",
                version,
                "source",
            ] => !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()),
            _ => false,
        };
        if !valid_route
            || query.len() > 2
            || query
                .iter()
                .any(|(key, value)| !["limit", "offset", "hash"].contains(key) || value.len() > 64)
        {
            return Err(KnowledgeError::Unavailable);
        }
        let mut url = self.endpoint(segments)?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(key, value)| (*key, value)));
        }
        let path = format!("/{}", segments.join("/"));
        let _permit = tokio::time::timeout(Duration::from_secs(2), self.requests.acquire())
            .await
            .map_err(|_| KnowledgeError::Unavailable)?
            .map_err(|_| KnowledgeError::Unavailable)?;
        let request = self.signed_request(
            Method::GET,
            url,
            &path,
            actor.headers(IdentityUse::Catalog)?,
            vec![],
        )?;
        let mut response = self
            .http
            .execute(request)
            .await
            .map_err(|_| KnowledgeError::Unavailable)?;
        let status = response.status();
        // In particular, do not follow or return redirect destinations or auth challenges.
        if status.is_redirection() || status == StatusCode::UNAUTHORIZED {
            return Err(KnowledgeError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(KnowledgeError::InvalidResponse);
        }
        let total_count = response.headers().get("x-total-count").cloned();
        let retry_after = response.headers().get("retry-after").cloned();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| KnowledgeError::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(KnowledgeError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let payload =
            serde_json::from_slice(&bytes).map_err(|_| KnowledgeError::InvalidResponse)?;
        Ok(KnowledgeReadResponse {
            status,
            payload,
            total_count,
            retry_after,
        })
    }

    pub async fn list_datasets(
        &self,
        actor: &KnowledgeActor,
    ) -> Result<Vec<Value>, KnowledgeError> {
        tokio::time::timeout(OPERATION_TIMEOUT, self.catalog(actor))
            .await
            .map_err(|_| KnowledgeError::Unavailable)?
    }

    async fn catalog(&self, actor: &KnowledgeActor) -> Result<Vec<Value>, KnowledgeError> {
        let mut catalog = Catalog::default();
        loop {
            let mut url = self.endpoint(&["api", "v1", "knowledge", "datasets"])?;
            url.query_pairs_mut().append_pair("limit", "200");
            if let Some(cursor) = &catalog.cursor {
                url.query_pairs_mut().append_pair("cursor", cursor);
            }
            let (payload, cursor, bytes) = self
                .request(
                    Method::GET,
                    url,
                    DATASET_DETAIL_PATH,
                    actor.headers(IdentityUse::Catalog)?,
                    vec![],
                )
                .await?;
            if catalog.append(payload, cursor, bytes)? {
                return Ok(catalog.rows);
            }
        }
    }

    /// Current KS ACL, suitable for history-source checks. Never infer visibility
    /// from a tenant match, saved citation, or an Agent's prior grant.
    pub async fn visible_dataset_ids(
        &self,
        actor: &KnowledgeActor,
    ) -> Result<BTreeSet<String>, KnowledgeError> {
        self.list_datasets(actor)
            .await?
            .iter()
            .map(dataset_id)
            .collect()
    }

    /// Request-local document ACL. Unknown response IDs poison the whole grant.
    pub async fn authorize_documents(
        &self,
        actor: &KnowledgeActor,
        dataset: &str,
        document_ids: &[String],
    ) -> Result<BTreeSet<String>, KnowledgeError> {
        let requested: BTreeSet<String> = document_ids
            .iter()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .collect();
        if requested.len() > MAX_DATASETS {
            return Err(KnowledgeError::InvalidResponse);
        }
        tokio::time::timeout(OPERATION_TIMEOUT, async {
            let ordered: Vec<String> = requested.into_iter().collect();
            let mut allowed = BTreeSet::new();
            for batch in ordered.chunks(500) {
                let payload = self
                    .post_authorization(
                        actor,
                        "documents",
                        json!({"dataset_id": dataset, "document_ids": batch}),
                    )
                    .await?;
                allowed.extend(document_grants(&payload, batch)?);
            }
            Ok(allowed)
        })
        .await
        .map_err(|_| KnowledgeError::Unavailable)?
    }

    /// Exact immutable source-version ACL, separate from dataset/document access.
    pub async fn authorize_document_sources(
        &self,
        actor: &KnowledgeActor,
        dataset: &str,
        references: &[DocumentSourceReference],
    ) -> Result<BTreeSet<DocumentSourceReference>, KnowledgeError> {
        if references.len() > MAX_DATASETS {
            return Err(KnowledgeError::InvalidResponse);
        }
        let requested: BTreeSet<_> = references
            .iter()
            .map(normalize_reference)
            .collect::<Result<_, _>>()?;
        tokio::time::timeout(OPERATION_TIMEOUT, async {
            let ordered: Vec<_> = requested.into_iter().collect();
            let mut allowed = BTreeSet::new();
            for batch in ordered.chunks(500) {
                let payload = self
                    .post_authorization(
                        actor,
                        "document-sources",
                        json!({"dataset_id": dataset, "references": batch}),
                    )
                    .await?;
                allowed.extend(source_grants(&payload, batch)?);
            }
            Ok(allowed)
        })
        .await
        .map_err(|_| KnowledgeError::Unavailable)?
    }

    async fn post_authorization(
        &self,
        actor: &KnowledgeActor,
        resource: &str,
        payload: Value,
    ) -> Result<Value, KnowledgeError> {
        let body = serde_json::to_vec(&payload).map_err(|_| KnowledgeError::InvalidResponse)?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(KnowledgeError::InvalidResponse);
        }
        let url = self.endpoint(&["api", "v1", "internal", "knowledge", resource, "authorize"])?;
        let path = format!("/api/v1/internal/knowledge/{resource}/authorize");
        self.request(
            Method::POST,
            url,
            &path,
            actor.headers(IdentityUse::Catalog)?,
            body,
        )
        .await
        .map(|(payload, _, _)| payload)
    }

    pub async fn resolve(
        &self,
        actor: &KnowledgeActor,
        bindings: &[Value],
    ) -> Result<Vec<Value>, AgentKnowledgeAuthorizationError> {
        resolve_bindings(Some(self), actor, bindings).await
    }

    pub async fn require_dataset_owner(
        &self,
        actor: &KnowledgeActor,
        dataset: &str,
    ) -> Result<(), DatasetImpactAuthorityError> {
        let unavailable = DatasetImpactAuthorityError { status_code: 503 };
        if !actor.authenticated || actor.tenant_id == "public" {
            return Err(unavailable);
        }
        let url = self
            .endpoint(&["api", "v1", "knowledge", "datasets", dataset])
            .map_err(|_| unavailable)?;
        // ASGI verifies the decoded path; fixed route segments and a single checked ID
        // keep both the wire path and canonical path bound to the intended resource.
        let path = format!("{DATASET_DETAIL_PATH}/{dataset}");
        let response = self
            .request(
                Method::GET,
                url,
                &path,
                actor.headers(IdentityUse::Owner).map_err(|_| unavailable)?,
                vec![],
            )
            .await;
        match response {
            Ok((payload, _, _)) => validate_owner(&payload, dataset, &actor.tenant_id),
            Err(KnowledgeError::UpstreamStatus(status @ (403 | 404))) => {
                Err(DatasetImpactAuthorityError {
                    status_code: status,
                })
            }
            Err(_) => Err(unavailable),
        }
    }
}

fn normalize_reference(
    reference: &DocumentSourceReference,
) -> Result<DocumentSourceReference, KnowledgeError> {
    let document_id = reference.document_id.trim().to_owned();
    let source_hash = reference.source_hash.trim().to_ascii_lowercase();
    if document_id.is_empty()
        || reference.source_version <= 0
        || source_hash.len() != 64
        || !source_hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(KnowledgeError::InvalidResponse);
    }
    Ok(DocumentSourceReference {
        document_id,
        source_version: reference.source_version,
        source_hash,
    })
}
fn document_grants(
    payload: &Value,
    requested: &[String],
) -> Result<BTreeSet<String>, KnowledgeError> {
    let raw = payload
        .get("allowed_document_ids")
        .and_then(Value::as_array)
        .ok_or(KnowledgeError::InvalidResponse)?;
    let allowed: BTreeSet<String> = raw
        .iter()
        .map(|id| {
            id.as_str()
                .map(str::to_owned)
                .ok_or(KnowledgeError::InvalidResponse)
        })
        .collect::<Result<_, _>>()?;
    if allowed.iter().any(|id| !requested.contains(id)) {
        return Err(KnowledgeError::InvalidResponse);
    }
    Ok(allowed)
}
fn source_grants(
    payload: &Value,
    requested: &[DocumentSourceReference],
) -> Result<BTreeSet<DocumentSourceReference>, KnowledgeError> {
    let rows = payload
        .get("allowed_references")
        .and_then(Value::as_array)
        .ok_or(KnowledgeError::InvalidResponse)?;
    let mut allowed = BTreeSet::new();
    for row in rows {
        // Do not normalize authority responses: changed hashes/versions must fail.
        let reference = DocumentSourceReference {
            document_id: row
                .get("document_id")
                .and_then(Value::as_str)
                .ok_or(KnowledgeError::InvalidResponse)?
                .to_owned(),
            source_version: row
                .get("source_version")
                .and_then(Value::as_i64)
                .ok_or(KnowledgeError::InvalidResponse)?,
            source_hash: row
                .get("source_hash")
                .and_then(Value::as_str)
                .ok_or(KnowledgeError::InvalidResponse)?
                .to_owned(),
        };
        if !requested.contains(&reference) {
            return Err(KnowledgeError::InvalidResponse);
        }
        allowed.insert(reference);
    }
    Ok(allowed)
}

/// Empty/malformed-only bindings require no configured KS or internal secret.
/// Otherwise every requested ID must be granted, with original binding order/data kept.
pub async fn resolve_bindings(
    service: Option<&KnowledgeService>,
    actor: &KnowledgeActor,
    bindings: &[Value],
) -> Result<Vec<Value>, AgentKnowledgeAuthorizationError> {
    let normalized: Vec<Value> = bindings
        .iter()
        .filter(|b| b.is_object() && b.get("dataset_id").is_some_and(truthy))
        .cloned()
        .collect();
    if normalized.is_empty() {
        return Ok(normalized);
    }
    let service = service.ok_or(AgentKnowledgeAuthorizationError)?;
    if normalized.len() > MAX_DATASETS {
        return Err(AgentKnowledgeAuthorizationError);
    }
    let ids: Vec<String> = normalized
        .iter()
        .map(|row| python_string(&row["dataset_id"]))
        .collect();
    let body = serde_json::to_vec(
        &json!({"dataset_ids": ids, "is_tenant_admin": actor.is_tenant_admin()}),
    )
    .map_err(|_| AgentKnowledgeAuthorizationError)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(AgentKnowledgeAuthorizationError);
    }
    let url = service
        .endpoint(&[
            "api",
            "v1",
            "internal",
            "knowledge",
            "datasets",
            "authorize",
        ])
        .map_err(|_| AgentKnowledgeAuthorizationError)?;
    let (payload, _, _) = service
        .request(
            Method::POST,
            url,
            AUTHORIZE_DATASETS_PATH,
            actor
                .headers(IdentityUse::Authorization)
                .map_err(|_| AgentKnowledgeAuthorizationError)?,
            body,
        )
        .await
        .map_err(|_| AgentKnowledgeAuthorizationError)?;
    require_allowed(&payload, &ids)?;
    Ok(normalized)
}

fn require_allowed(
    payload: &Value,
    ids: &[String],
) -> Result<(), AgentKnowledgeAuthorizationError> {
    let raw = payload
        .get("allowed_dataset_ids")
        .and_then(Value::as_array)
        .ok_or(AgentKnowledgeAuthorizationError)?;
    let allowed: BTreeSet<&str> = raw
        .iter()
        .map(|id| id.as_str().ok_or(AgentKnowledgeAuthorizationError))
        .collect::<Result<_, _>>()?;
    if ids.iter().all(|id| allowed.contains(id.as_str())) {
        Ok(())
    } else {
        Err(AgentKnowledgeAuthorizationError)
    }
}
fn validate_owner(
    payload: &Value,
    dataset: &str,
    tenant: &str,
) -> Result<(), DatasetImpactAuthorityError> {
    if !payload.is_object()
        || payload.get("dataset_id").map(python_string).as_deref() != Some(dataset)
    {
        return Err(DatasetImpactAuthorityError { status_code: 503 });
    }
    if payload.get("tenant_id").map(python_string).as_deref() != Some(tenant)
        || payload["my_permission"] != "owner"
    {
        return Err(DatasetImpactAuthorityError { status_code: 403 });
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "Explicit canonical HMAC-v2 signed components"
)]
fn signature(
    config: &KnowledgeConfig,
    method: &Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body: &[u8],
    request_id: &str,
    timestamp: &str,
) -> Result<String, KnowledgeError> {
    if !path.starts_with("/api/v1/") || path.contains(['\r', '\n', '?', '#']) {
        return Err(KnowledgeError::Unavailable);
    }
    let mut identity = BTreeMap::new();
    for key in [
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
        if let Some(value) = headers.get(key) {
            identity.insert(
                key,
                value.to_str().map_err(|_| KnowledgeError::Unavailable)?,
            );
        }
    }
    let identity = identity
        .into_iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let hash = format!("{:x}", Sha256::digest(body));
    let canonical = [
        "gateway",
        "knowledge-service",
        method.as_str(),
        path,
        query,
        &hash,
        &identity,
        request_id,
        timestamp,
        &config.key_id,
    ]
    .join("\n");
    let mut mac = Hmac::<Sha256>::new_from_slice(config.secret.as_bytes())
        .map_err(|_| KnowledgeError::Configuration)?;
    mac.update(canonical.as_bytes());
    let digest = mac.finalize().into_bytes();
    let hex = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok(format!(
        "v2:{}:{request_id}:{timestamp}:{hash}:{hex}",
        config.key_id
    ))
}

#[derive(Default)]
struct Catalog {
    rows: Vec<Value>,
    positions: BTreeMap<String, usize>,
    seen: BTreeSet<String>,
    cursor: Option<String>,
    pages: usize,
    bytes: usize,
}
impl Catalog {
    /// Returns true only when all pages are complete; no partial catalog escapes.
    fn append(
        &mut self,
        payload: Value,
        cursor: Option<String>,
        bytes: usize,
    ) -> Result<bool, KnowledgeError> {
        self.pages += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.pages > MAX_CATALOG_PAGES || self.bytes > MAX_OPERATION_BYTES {
            return Err(KnowledgeError::InvalidResponse);
        }
        let rows = match &payload {
            Value::Array(rows) => rows.as_slice(),
            Value::Object(object) => match object.get("datasets").or_else(|| object.get("data")) {
                Some(Value::Array(rows)) => rows.as_slice(),
                None => &[],
                _ => return Err(KnowledgeError::InvalidResponse),
            },
            _ => return Err(KnowledgeError::InvalidResponse),
        };
        for row in rows {
            let id = dataset_id(row)?;
            if let Some(position) = self.positions.get(&id) {
                self.rows[*position] = row.clone();
            } else {
                if self.rows.len() == MAX_DATASETS {
                    return Err(KnowledgeError::InvalidResponse);
                }
                self.positions.insert(id, self.rows.len());
                self.rows.push(row.clone());
            }
        }
        self.cursor = cursor.filter(|c| !c.is_empty());
        if let Some(cursor) = &self.cursor
            && (cursor.len() > 1024
                || !self.seen.insert(cursor.clone())
                || self.pages == MAX_CATALOG_PAGES)
        {
            return Err(KnowledgeError::InvalidResponse);
        }
        Ok(self.cursor.is_none())
    }
}
fn dataset_id(row: &Value) -> Result<String, KnowledgeError> {
    let id = row
        .get("dataset_id")
        .filter(|v| v.is_string() || v.is_number())
        .ok_or(KnowledgeError::InvalidResponse)?;
    Ok(python_string(id))
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn python_string(value: &Value) -> String {
    match value {
        Value::String(v) => v.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        _ => value.to_string(),
    }
}
fn count(value: Option<&Value>) -> Result<i64, KnowledgeError> {
    match value.filter(|v| truthy(v)) {
        None => Ok(0),
        Some(Value::Bool(v)) => Ok(i64::from(*v)),
        Some(Value::Number(v)) => v
            .as_i64()
            .or_else(|| {
                v.as_f64()
                    .filter(|f| *f >= i64::MIN as f64 && *f < i64::MAX as f64)
                    .map(|f| f as i64)
            })
            .ok_or(KnowledgeError::InvalidResponse),
        Some(Value::String(v)) => v
            .trim()
            .parse()
            .map_err(|_| KnowledgeError::InvalidResponse),
        _ => Err(KnowledgeError::InvalidResponse),
    }
}
#[derive(Serialize)]
pub struct DatasetInfo {
    pub dataset_id: String,
    pub name: String,
    pub description: Option<String>,
    pub document_count: i64,
    pub chunk_count: i64,
    pub embedding_model: Option<String>,
    pub is_multimodal: bool,
}
fn optional_string(value: Option<&Value>) -> Result<Option<String>, KnowledgeError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(KnowledgeError::InvalidResponse),
    }
}
pub fn project_datasets(rows: &[Value]) -> Result<Vec<DatasetInfo>, KnowledgeError> {
    rows.iter()
        .filter(|row| row.is_object())
        .map(|row| {
            let stats = row.get("statistics").and_then(Value::as_object);
            let model = optional_string(row.get("embedding_model"))?;
            Ok(DatasetInfo {
                dataset_id: row
                    .get("dataset_id")
                    .filter(|v| truthy(v))
                    .map(python_string)
                    .unwrap_or_default(),
                name: row
                    .get("name")
                    .filter(|v| truthy(v))
                    .map(python_string)
                    .unwrap_or_default(),
                description: optional_string(row.get("description"))?,
                document_count: count(
                    stats
                        .and_then(|s| s.get("document_count"))
                        .or_else(|| row.get("document_count")),
                )?,
                chunk_count: count(
                    stats
                        .and_then(|s| s.get("segment_count"))
                        .or_else(|| row.get("segment_count"))
                        .or_else(|| row.get("chunk_count")),
                )?,
                is_multimodal: model
                    .as_deref()
                    .is_some_and(|m| MULTIMODAL_MODELS.contains(&m)),
                embedding_model: model,
            })
        })
        .collect()
}

#[derive(Clone)]
struct KnowledgeRoutes {
    auth: Arc<AuthService>,
    service: Option<Arc<KnowledgeService>>,
    identity: Option<Arc<dyn crate::migration_content::ContentAccess>>,
}
/// Mount with the existing auth/router. None yields authenticated 503, never empty 200.
pub fn router(auth: Arc<AuthService>, service: Option<Arc<KnowledgeService>>) -> Router {
    router_with_identity(auth, service, None)
}
pub fn router_with_identity(
    auth: Arc<AuthService>,
    service: Option<Arc<KnowledgeService>>,
    identity: Option<Arc<dyn crate::migration_content::ContentAccess>>,
) -> Router {
    Router::new()
        .route(
            "/api/v1/assistant/datasets",
            get(datasets).head(super::read_method_not_allowed),
        )
        .with_state(KnowledgeRoutes {
            auth,
            service,
            identity,
        })
}
async fn datasets(State(state): State<KnowledgeRoutes>, headers: HeaderMap) -> Response {
    let principal = match super::resolve_read_identity(
        &state.auth,
        state.identity.as_deref(),
        &headers,
    )
    .await
    {
        Ok(user) => user,
        Err(response) => return response,
    };
    let actor = match KnowledgeActor::from_identity(&principal) {
        Ok(actor) => actor,
        Err(_) => return super::error(StatusCode::UNAUTHORIZED, "Authentication required"),
    };
    let Some(service) = state.service else {
        return super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Knowledge service is unavailable",
        );
    };
    match service
        .list_datasets(&actor)
        .await
        .and_then(|rows| project_datasets(&rows))
    {
        Ok(datasets) => Json(json!({"datasets": datasets})).into_response(),
        Err(_) => super::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Knowledge service is unavailable",
        ),
    }
}

#[cfg(test)]
mod read_identity_tests {
    use super::*;
    #[test]
    fn resolved_guest_retains_scope_without_authority_promotion() {
        let actor = KnowledgeActor::from_identity(&crate::migration_content::ContentIdentity {
            tenant_id: "public".into(),
            user_id: "guest_cookie".into(),
            is_authenticated: false,
            roles: vec!["admin".into()],
            permissions: vec!["admin:*".into()],
            tier: "admin".into(),
            allowed_models: None,
        })
        .unwrap();
        assert!(!actor.authenticated);
        assert_eq!(actor.roles, vec!["guest"]);
        assert_eq!(actor.tier, "anonymous");
        assert_eq!(actor.tenant_id, "public");
    }
    #[test]
    fn resolved_authenticated_identity_keeps_real_scope_and_permissions() {
        let actor = KnowledgeActor::from_identity(&crate::migration_content::ContentIdentity {
            tenant_id: "tenant".into(),
            user_id: "owner".into(),
            is_authenticated: true,
            roles: vec!["premium".into()],
            permissions: vec!["dataset:read".into()],
            tier: "standard".into(),
            allowed_models: None,
        })
        .unwrap();
        assert!(actor.authenticated);
        assert_eq!(actor.tier, "premium");
        assert!(actor.roles.contains(&"dataset:read".to_string()));
        assert_eq!(actor.user_id, "owner");
    }
}
