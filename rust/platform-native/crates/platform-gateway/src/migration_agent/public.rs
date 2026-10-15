//! Published Agent access: audience ACL, browser grants, one-way API token hashes,
//! atomic channel limits and immutable session/version pins.
use super::{
    Actor, AgentError, AgentFuture, Scope,
    routes::{AgentState, event_response},
    spec,
    studio::{AgentRepository, require_active_publication},
    text,
};
use axum::{
    Json,
    body::to_bytes,
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
pub trait PublicIdentityResolver: Send + Sync {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> AgentFuture<'a, Actor>;
}
/// The host supplies its existing authenticated/guest identity resolver. Public
/// tenant identity is read from a publication only after audience authorization.
#[derive(Clone)]
pub struct PublicRuntime {
    pub identities: Arc<dyn PublicIdentityResolver>,
    pub redis: redis::aio::ConnectionManager,
    pub embed_secret: Arc<String>,
    pub storage: Option<Arc<dyn crate::migration_content::storage::ObjectStorage>>,
    pub max_attachment_bytes: usize,
}
fn random_token(prefix: &str) -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}
fn digest(s: &str) -> String {
    spec::hex(&Sha256::digest(s.as_bytes()))
}
pub fn normalized_origin(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}
fn origins(p: &Value) -> Vec<String> {
    let values: BTreeSet<_> = p["policy"]["allowed_origins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(normalized_origin)
        .collect();
    if values.iter().any(|v| v.contains('*')) {
        vec![]
    } else {
        values.into_iter().collect()
    }
}
fn cookie(headers: &HeaderMap, key: &str) -> String {
    headers
        .get("cookie")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .find_map(|part| {
            let (k, v) = part.trim().split_once('=')?;
            (k == key).then(|| v.to_owned())
        })
        .unwrap_or_default()
}
fn sign(secret: &str, bytes: &[u8]) -> Result<Vec<u8>, AgentError> {
    if secret.is_empty() {
        return Err(AgentError::code(503, "AGENT_EMBED_SIGNING_UNAVAILABLE"));
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| AgentError::code(503, "AGENT_EMBED_SIGNING_UNAVAILABLE"))?;
    mac.update(bytes);
    Ok(mac.finalize().into_bytes().to_vec())
}
impl PublicRuntime {
    pub fn issue_embed_token(
        &self,
        public_id: &str,
        origin: &str,
        nonce: &str,
        client_ip: &str,
    ) -> Result<String, AgentError> {
        let abuse = sign(
            &self.embed_secret,
            format!("{public_id}\n{origin}\n{client_ip}").as_bytes(),
        )?;
        let payload = json!({"v":"agent-embed/v1","public_id":public_id,"origin":origin,"nonce":nonce,"sub":URL_SAFE_NO_PAD.encode(&abuse[..18]),"exp":Utc::now().timestamp()+300});
        let encoded = URL_SAFE_NO_PAD.encode(spec::canonical(&payload));
        let signature = URL_SAFE_NO_PAD.encode(sign(&self.embed_secret, encoded.as_bytes())?);
        Ok(format!("e1.{encoded}.{signature}"))
    }
    pub fn verify_embed_token(
        &self,
        headers: &HeaderMap,
        token: &str,
        public_id: &str,
    ) -> Result<Value, AgentError> {
        let invalid = || AgentError::code(401, "AGENT_EMBED_TOKEN_INVALID");
        let mut parts = token.splitn(3, '.');
        if parts.next() != Some("e1") {
            return Err(invalid());
        }
        let encoded = parts.next().ok_or_else(invalid)?;
        let signature = parts.next().ok_or_else(invalid)?;
        let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| invalid())?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(self.embed_secret.as_bytes()).map_err(|_| invalid())?;
        mac.update(encoded.as_bytes());
        mac.verify_slice(&signature).map_err(|_| invalid())?;
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
        let p: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let origin = normalized_origin(
            headers
                .get("x-agent-embed-origin")
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        );
        if p["v"] != "agent-embed/v1"
            || p["public_id"] != public_id
            || p["exp"].as_i64().unwrap_or(0) < Utc::now().timestamp()
            || normalized_origin(text(&p, "origin")).as_deref() != Some(text(&p, "origin"))
            || origin.as_deref() != Some(text(&p, "origin"))
            || text(&p, "nonce").is_empty()
            || cookie(headers, "ag_embed_session") != text(&p, "nonce")
            || text(&p, "sub").is_empty()
        {
            return Err(invalid());
        }
        Ok(p)
    }
    pub async fn resolve_browser(
        &self,
        repository: &AgentRepository,
        mut actor: Actor,
        headers: &HeaderMap,
        public_id: &str,
        channel: &str,
        embed_token: Option<&str>,
        pin: Option<&str>,
    ) -> Result<(Value, Actor), AgentError> {
        if !matches!(channel, "hosted" | "embed") {
            return Err(AgentError::code(422, "PUBLICATION_CHANNEL_INVALID"));
        }
        let authenticated = actor.tier != "anonymous" && !actor.roles.iter().any(|r| r == "guest");
        let browser = if channel == "embed" {
            let token = headers
                .get("x-agent-embed-token")
                .and_then(|v| v.to_str().ok())
                .or(embed_token)
                .unwrap_or("");
            let claim = self.verify_embed_token(headers, token, public_id)?;
            if !authenticated {
                actor.user_id = format!("agent-embed:{}", text(&claim, "sub"));
                actor.tenant_id = "public".into();
            }
            true
        } else {
            false
        };
        let mut c = repository.pool.acquire().await?;
        let p: Value = sqlx::query_scalar(
            "SELECT to_jsonb(p) FROM agent_publications p WHERE public_id=$1::uuid",
        )
        .bind(super::uuid(public_id)?)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(|| AgentError::code(409, "PUBLICATION_DISABLED"))?;
        require_active_publication(&p)?;
        if p["channel"] != channel {
            return Err(AgentError::code(409, "PUBLICATION_CHANNEL_MISMATCH"));
        }
        let matches = authenticated && actor.tenant_id == text(&p, "tenant_id");
        let mode = text(&p, "auth_mode");
        if matches!(mode, "private" | "tenant") && !authenticated {
            return Err(AgentError::code(401, "PUBLICATION_AUTHENTICATION_REQUIRED"));
        }
        let mut role = "viewer".to_owned();
        let allowed = match mode {
            "public" => true,
            "tenant" => matches,
            "token" => channel == "embed" && browser,
            "private" if matches => {
                let member:Option<String>=sqlx::query_scalar("SELECT role FROM agent_members WHERE tenant_id=$1 AND agent_id=$2::uuid AND principal_type='user' AND principal_id=$3").bind(text(&p,"tenant_id")).bind(text(&p,"agent_id")).bind(&actor.user_id).fetch_optional(&mut *c).await?;
                role = if actor.is_admin() {
                    "owner".into()
                } else {
                    member.clone().unwrap_or_default()
                };
                actor.is_admin() || member.is_some()
            }
            _ => false,
        };
        if !allowed {
            return Err(AgentError::code(403, "PUBLICATION_ACCESS_DENIED"));
        }
        let tenant = text(&p, "tenant_id").to_owned();
        let a:Value=sqlx::query_scalar("SELECT to_jsonb(a) FROM agents a WHERE tenant_id=$1 AND agent_id=$2::uuid AND deleted_at IS NULL").bind(&tenant).bind(text(&p,"agent_id")).fetch_optional(&mut *c).await?.ok_or_else(||AgentError::code(409,"AGENT_RUNTIME_AGENT_UNAVAILABLE"))?;
        let version = pin.unwrap_or(text(&p, "version_id")).to_owned();
        let resolution = repository
            .version_material(&mut c, &tenant, &a, &version, &role, p)
            .await?;
        if !authenticated {
            actor.tenant_id = tenant;
        }
        Ok((resolution, actor))
    }
    pub async fn resolve_api(
        &self,
        repository: &AgentRepository,
        raw: &str,
        publication: &str,
        required: &str,
        pin: Option<&str>,
    ) -> Result<(Value, Actor), AgentError> {
        if !raw.starts_with("agt_") || raw.len() > 256 {
            return Err(AgentError::code(401, "AGENT_RUNTIME_TOKEN_INVALID"));
        }
        let mut tx = repository.pool.begin().await?;
        let token:Value=sqlx::query_scalar("SELECT to_jsonb(t)||jsonb_build_object('publication',to_jsonb(p)) FROM agent_api_tokens t JOIN agent_publications p ON p.tenant_id=t.tenant_id AND p.publication_id=t.publication_id WHERE t.token_hash=$1 AND t.publication_id=$2::uuid FOR UPDATE OF t").bind(digest(raw)).bind(super::uuid(publication)?).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(401,"AGENT_RUNTIME_TOKEN_INVALID"))?;
        if !token["revoked_at"].is_null()
            || token["expires_at"].as_str().is_some_and(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map(|t| t.with_timezone(&Utc) <= Utc::now())
                    .unwrap_or(true)
            })
        {
            return Err(AgentError::code(401, "AGENT_RUNTIME_TOKEN_INVALID"));
        }
        let p = &token["publication"];
        require_active_publication(p)?;
        if p["channel"] != "api" {
            return Err(AgentError::code(409, "PUBLICATION_CHANNEL_MISMATCH"));
        }
        if token["scopes"]
            .as_array()
            .is_none_or(|a| !a.iter().any(|v| v == required))
        {
            return Err(AgentError::code(403, "AGENT_RUNTIME_TOKEN_SCOPE_FORBIDDEN"));
        }
        sqlx::query("UPDATE agent_api_tokens SET last_used_at=NOW() WHERE tenant_id=$1 AND token_id=$2::uuid").bind(text(&token,"tenant_id")).bind(text(&token,"token_id")).execute(&mut *tx).await?;
        let tenant = text(&token, "tenant_id");
        let a:Value=sqlx::query_scalar("SELECT to_jsonb(a) FROM agents a WHERE tenant_id=$1 AND agent_id=$2::uuid AND deleted_at IS NULL").bind(tenant).bind(text(p,"agent_id")).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(409,"AGENT_RUNTIME_AGENT_UNAVAILABLE"))?;
        let mut resolution = repository
            .version_material(
                &mut tx,
                tenant,
                &a,
                pin.unwrap_or(text(p, "version_id")),
                "runtime_token",
                p.clone(),
            )
            .await?;
        resolution["api_token"] = json!({"token_id":token["token_id"],"scopes":token["scopes"]});
        let actor = Actor {
            tenant_id: tenant.into(),
            user_id: format!("agent-token:{}", text(&token, "token_id")),
            tier: "service".into(),
            roles: vec!["agent_runtime".into()],
        };
        tx.commit().await?;
        Ok((resolution, actor))
    }
    pub async fn enforce_limits(
        &self,
        repository: &AgentRepository,
        publication: &Value,
        principal: &str,
        ip: &str,
    ) -> Result<(), AgentError> {
        let usage = super::governance::runtime_usage(repository, publication).await?;
        for code in usage["exceeded"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if code != "AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED" {
                return Err(AgentError::code(429, code));
            }
        }
        let policy = &usage["policy"];
        let raw = &publication["policy"];
        let definitions = [
            (
                "requests_per_minute",
                "principal_requests_per_minute",
                30,
                10000,
                60,
            ),
            (
                "requests_per_day",
                "principal_requests_per_day",
                1000,
                10000000,
                86400,
            ),
            (
                "ip_requests_per_minute",
                "ip_requests_per_minute",
                60,
                10000,
                60,
            ),
            (
                "ip_requests_per_day",
                "ip_requests_per_day",
                2000,
                10000000,
                86400,
            ),
            (
                "publication_requests_per_minute",
                "publication_requests_per_minute",
                300,
                100000,
                60,
            ),
            (
                "publication_requests_per_day",
                "publication_requests_per_day",
                10000,
                100000000,
                86400,
            ),
        ];
        let publication_hash = digest(text(publication, "publication_id"));
        let principal_hash = digest(principal);
        let ip_hash = digest(if ip.is_empty() { "unknown" } else { ip });
        let tag = format!("{{{}}}", &publication_hash[..32]);
        let keys = [
            format!(
                "agent:channel:{tag}:principal:{}:minute",
                &principal_hash[..32]
            ),
            format!(
                "agent:channel:{tag}:principal:{}:day",
                &principal_hash[..32]
            ),
            format!("agent:channel:{tag}:ip:{}:minute", &ip_hash[..32]),
            format!("agent:channel:{tag}:ip:{}:day", &ip_hash[..32]),
            format!("agent:channel:{tag}:publication:minute"),
            format!("agent:channel:{tag}:publication:day"),
        ];
        let script = redis::Script::new(
            "for i=1,#KEYS do local current=tonumber(redis.call('GET',KEYS[i]) or '0'); local limit=tonumber(ARGV[(i-1)*2+1]); if current>=limit then return i end end; for i=1,#KEYS do local value=redis.call('INCR',KEYS[i]); if value==1 then redis.call('EXPIRE',KEYS[i],tonumber(ARGV[(i-1)*2+2])) end end; return 0",
        );
        let mut invoke = script.prepare_invoke();
        for key in keys {
            invoke.key(key);
        }
        for (raw_key, policy_key, default, max, ttl) in definitions {
            let bound = policy[policy_key].as_i64().unwrap_or(default);
            let limit = raw[raw_key]
                .as_i64()
                .unwrap_or(bound)
                .clamp(1, max)
                .min(bound);
            invoke.arg(limit).arg(ttl);
        }
        let bucket: i64 = invoke
            .invoke_async(&mut self.redis.clone())
            .await
            .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_QUOTA_UNAVAILABLE"))?;
        if bucket > 0 {
            return Err(AgentError::code(
                429,
                if [2, 4, 6].contains(&bucket) {
                    "AGENT_RUNTIME_QUOTA_EXCEEDED"
                } else {
                    "AGENT_RUNTIME_RATE_LIMITED"
                },
            ));
        }
        Ok(())
    }
    pub async fn dispatch(
        &self,
        state: &AgentState,
        query: HashMap<String, String>,
        request: Request,
    ) -> Response {
        match self.dispatch_inner(state, query, request).await {
            Ok(r) => r,
            Err(e) => e.into_response(),
        }
    }
    async fn dispatch_inner(
        &self,
        state: &AgentState,
        query: HashMap<String, String>,
        request: Request,
    ) -> Result<Response, AgentError> {
        if request.method() == "POST" && request.uri().path().ends_with("/attachments") {
            return self.upload_attachment(state, query, request).await;
        }
        let path = request.uri().path().to_owned();
        let method = request.method().as_str().to_owned();
        let headers = request.headers().clone();
        let ip = request
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|v| v.0.ip().to_string())
            .unwrap_or_default();
        let secure = request.uri().scheme_str() == Some("https");
        let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
            .await
            .map_err(|_| AgentError::code(413, "REQUEST_TOO_LARGE"))?;
        let payload = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| AgentError::code(422, "INVALID_JSON"))?
        };
        let req_id = uuid::Uuid::new_v4().to_string();
        if let Some(public) = path.strip_prefix("/embed/agents/") {
            if method != "GET" {
                return Err(AgentError::code(405, "METHOD_NOT_ALLOWED"));
            }
            let p:Value=sqlx::query_scalar("SELECT to_jsonb(p)||jsonb_build_object('name',a.name) FROM agent_publications p JOIN agents a ON a.tenant_id=p.tenant_id AND a.agent_id=p.agent_id AND a.deleted_at IS NULL WHERE p.public_id=$1::uuid").bind(super::uuid(public)?).fetch_optional(&state.repository.pool).await?.ok_or_else(||AgentError::code(404,"PUBLICATION_DISABLED"))?;
            require_active_publication(&p)?;
            if p["channel"] != "embed" {
                return Err(AgentError::code(404, "PUBLICATION_CHANNEL_MISMATCH"));
            }
            let allowed = origins(&p);
            let parent = headers
                .get("origin")
                .and_then(|v| v.to_str().ok())
                .and_then(normalized_origin)
                .or_else(|| {
                    headers
                        .get("referer")
                        .and_then(|v| v.to_str().ok())
                        .and_then(normalized_origin)
                })
                .ok_or_else(|| AgentError::code(403, "AGENT_EMBED_ORIGIN_FORBIDDEN"))?;
            if !allowed.contains(&parent) {
                return Err(AgentError::code(403, "AGENT_EMBED_ORIGIN_FORBIDDEN"));
            }
            let nonce = random_token("");
            let token = self.issue_embed_token(public, &parent, &nonce, &ip)?;
            let escape = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
                    .replace('\'', "&#x27;")
            };
            let body = format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><link rel=\"stylesheet\" href=\"/agent-embed.css\"><title>{}</title></head><body><main id=\"agent-embed-root\" data-public-id=\"{}\" data-parent-origin=\"{}\" data-embed-token=\"{}\"></main><script type=\"module\" src=\"/agent-embed.js\"></script></body></html>",
                escape(text(&p, "name")),
                escape(public),
                escape(&parent),
                escape(&token)
            );
            let cookie = format!(
                "ag_embed_session={nonce}; Max-Age=300; Path=/; HttpOnly; SameSite={}{}",
                if secure { "None" } else { "Lax" },
                if secure { "; Secure" } else { "" }
            );
            return Ok(Response::builder().header("content-type","text/html; charset=utf-8").header("set-cookie",cookie).header("cache-control","no-store").header("referrer-policy","no-referrer").header("permissions-policy","camera=(), microphone=(), geolocation=()").header("content-security-policy",format!("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; font-src 'self'; frame-ancestors {}; base-uri 'none'; form-action 'none'; object-src 'none'",allowed.join(" "))).body(axum::body::Body::from(body)).expect("embed response"));
        }
        if let Some(rest) = path.strip_prefix("/api/v1/publications/") {
            return self
                .tokens_route(state, &headers, rest, &method, payload)
                .await;
        }
        let (api, rest) = if let Some(rest) = path.strip_prefix("/api/v1/agent-runtime/") {
            (true, rest)
        } else {
            (
                false,
                path.strip_prefix("/api/v1/public/agents/")
                    .ok_or_else(|| AgentError::code(404, "NOT_FOUND"))?,
            )
        };
        if !state.runtime_enabled {
            return Err(AgentError::code(503, "AGENT_RUNTIME_DISABLED"));
        }
        reject_forgery(&headers, None)?;
        let parts: Vec<_> = rest.trim_matches('/').split('/').collect();
        let id = super::uuid(parts[0])?;
        let action = parts.get(1).copied().unwrap_or("");
        let channel = if api {
            "api"
        } else {
            payload["channel"]
                .as_str()
                .or_else(|| query.get("channel").map(String::as_str))
                .unwrap_or("hosted")
        };
        if method == "POST" {
            match action {
                "sessions" if !api => {
                    super::routes::closed(&payload, &["channel", "embed_token"])?;
                    if !payload["channel"].is_string() {
                        return Err(AgentError::code(422, "PUBLICATION_CHANNEL_INVALID"));
                    }
                }
                "chat" => {
                    let mut fields = vec![
                        "message",
                        "session_id",
                        "attachments",
                        "resume_run_id",
                        "resume_approval_id",
                    ];
                    if !api {
                        fields.extend(["channel", "embed_token"]);
                    }
                    super::routes::closed(&payload, &fields)?;
                    super::routes::bounded(&payload, "message", 1, 200000, true)?;
                    super::routes::bounded(&payload, "session_id", 1, 255, false)?;
                    super::routes::runtime_attachments(&payload)?;
                    if payload["resume_run_id"].is_null() != payload["resume_approval_id"].is_null()
                    {
                        return Err(AgentError::code(422, "INVALID_RESUME_IDENTITY"));
                    }
                }
                "feedback" => {
                    super::routes::closed(
                        &payload,
                        &["session_id", "rating", "comment", "channel", "embed_token"],
                    )?;
                    super::routes::bounded(&payload, "session_id", 1, 255, true)?;
                    super::routes::bounded(&payload, "comment", 0, 2000, false)?;
                }
                _ => {}
            }
            super::routes::bounded(&payload, "embed_token", 0, 2048, false)?;
        }
        let required = match action {
            "sessions" => "sessions:write",
            "attachments" => "attachments:write",
            "feedback" => "feedback:write",
            _ => "chat:write",
        };
        let actor = if api {
            Actor::default()
        } else {
            self.identities.resolve(&headers, &path).await?
        };
        let raw_token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .unwrap_or("");
        let (mut resolution, caller) = if api {
            self.resolve_api(&state.repository, raw_token, &id, required, None)
                .await?
        } else {
            self.resolve_browser(
                &state.repository,
                actor.clone(),
                &headers,
                &id,
                channel,
                payload["embed_token"].as_str(),
                None,
            )
            .await?
        };
        if method == "GET" && action.is_empty() {
            let snapshot = state.build_snapshot(&caller, &resolution, channel).await?;
            let p = &resolution["publication"];
            let version = &resolution["version"];
            let mut identity = super::object(&resolution["spec"]["identity"]);
            identity.as_object_mut().expect("identity").retain(|k, v| {
                [
                    "icon_url",
                    "theme_color",
                    "welcome_message",
                    "suggested_prompts",
                ]
                .contains(&k.as_str())
                    && !v.is_null()
            });
            return Ok(Json(json!({"public_id":id,"publication_id":p["publication_id"],"channel":channel,"auth_mode":p["auth_mode"],"name":resolution["agent"]["name"],"description":resolution["agent"]["description"],"identity":identity,"attachments":p["policy"]["attachments"].as_bool().unwrap_or(false),"version_number":version["version_number"],"capability_count":snapshot["capabilities"].as_array().map(Vec::len).unwrap_or(0),"knowledge_count":snapshot["knowledge"]["datasets"].as_array().map(Vec::len).unwrap_or(0),"release_gate_verified":!version["release_evaluation_id"].is_null(),"published_at":p["updated_at"],"request_id":req_id})).into_response());
        }
        let session = if action == "sessions" {
            uuid::Uuid::new_v4().to_string()
        } else {
            payload["session_id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
        };
        let scope = caller.scope(&session);
        let existing: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(s) FROM assistant.sessions s WHERE session_id=$1")
                .bind(&session)
                .fetch_optional(&state.repository.pool)
                .await?;
        if let Some(existing) = &existing {
            if existing["tenant_id"] != caller.tenant_id
                || existing["user_id"] != caller.user_id
                || existing["agent_id"] != resolution["agent"]["agent_id"]
                || existing["publication_id"] != resolution["publication"]["publication_id"]
                || existing["channel"] != channel
            {
                return Err(AgentError::code(409, "AGENT_RUNTIME_PIN_CONFLICT"));
            }
            if existing["agent_version_id"] != resolution["version"]["agent_version_id"] {
                resolution = if api {
                    self.resolve_api(
                        &state.repository,
                        raw_token,
                        &id,
                        required,
                        existing["agent_version_id"].as_str(),
                    )
                    .await?
                    .0
                } else {
                    self.resolve_browser(
                        &state.repository,
                        actor,
                        &headers,
                        &id,
                        channel,
                        payload["embed_token"].as_str(),
                        existing["agent_version_id"].as_str(),
                    )
                    .await?
                    .0
                };
            }
        }
        let mut reservation = None;
        if api && method == "POST" && parts.get(1) == Some(&"chat") {
            if let Some(key) = headers
                .get("idempotency-key")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if key.len() > 255 {
                    return Err(AgentError::code(
                        422,
                        "AGENT_RUNTIME_IDEMPOTENCY_KEY_INVALID",
                    ));
                }
                let mut normalized = payload.clone();
                for (k, v) in [
                    ("session_id", Value::Null),
                    ("attachments", json!([])),
                    ("resume_run_id", Value::Null),
                    ("resume_approval_id", Value::Null),
                ] {
                    if normalized.get(k).is_none() {
                        normalized[k] = v;
                    }
                }
                let key = RuntimeIdempotency {
                    tenant: caller.tenant_id.clone(),
                    publication: text(&resolution["publication"], "publication_id").into(),
                    principal: caller.user_id.clone(),
                    key: key.into(),
                    request_hash: spec::hash(&normalized),
                };
                let row = key.reserve(&state.repository.pool, &session).await?;
                if row["created"] != true {
                    return super::idempotency::replay(&row);
                }
                reservation = Some(super::idempotency::Reservation {
                    key,
                    pool: state.repository.pool.clone(),
                    armed: true,
                });
            }
        }
        self.enforce_limits(
            &state.repository,
            &resolution["publication"],
            &caller.user_id,
            &ip,
        )
        .await?;
        let snapshot = state.build_snapshot(&caller, &resolution, channel).await?;
        match (method.as_str(), &parts[1..]) {
            ("POST", ["sessions"]) => {
                state.bind_pin(&scope, &snapshot, None).await?;
                Ok((StatusCode::CREATED,Json(json!({"session_id":session,"agent_id":snapshot["agent_id"],"agent_version_id":snapshot["agent_version_id"],"publication_id":snapshot["publication"]["id"],"channel":channel,"runtime_fingerprint":format!("sha256:{}",spec::hash(&snapshot)),"request_id":req_id}))).into_response())
            }
            ("POST", ["chat", "stream"]) => {
                let message = payload["message"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.chars().count() <= 200000)
                    .ok_or_else(|| AgentError::code(422, "INVALID_MESSAGE"))?;
                if !payload["resume_run_id"].is_null() || !payload["resume_approval_id"].is_null() {
                    return Err(AgentError::code(409, "AGENT_RUNTIME_RESUME_NOT_AVAILABLE"));
                }
                let attachment_ids = super::routes::runtime_attachments(&payload)?;
                if !attachment_ids.is_empty() && snapshot["channel_policy"]["attachments"] != true {
                    return Err(AgentError::code(422, "AGENT_RUNTIME_ATTACHMENTS_FORBIDDEN"));
                }
                let attachments = state
                    .repository
                    .resolve_runtime_attachments(
                        &caller,
                        text(&resolution["publication"], "publication_id"),
                        channel,
                        &attachment_ids,
                    )
                    .await?;
                let attachment_refs = attachments
                    .iter()
                    .map(|v| text(v, "file_path").to_owned())
                    .collect();
                state.bind_pin(&scope, &snapshot, None).await?;
                let turn = state
                    .start_snapshot_turn_with_refs(
                        caller,
                        session.clone(),
                        message.into(),
                        snapshot.clone(),
                        attachment_refs,
                    )
                    .await?;
                let events = state
                    .control
                    .stream_thread_events(
                        scope.clone(),
                        turn.runtime_thread_id.clone(),
                        turn.after_sequence,
                        1000,
                        Some(turn.run_id.clone()),
                    )
                    .await?;
                let response = event_response(
                    events,
                    turn.runtime_thread_id.clone(),
                    Some(turn.run_id.clone()),
                    turn.after_sequence,
                    false,
                    None,
                    Some(&session),
                );
                let response = super::capture::capture_response(
                    state,
                    response,
                    &scope,
                    &turn.runtime_thread_id,
                    Some(&turn.run_id),
                    turn.after_sequence,
                    1000,
                    false,
                    None,
                    &req_id,
                    Some(snapshot),
                )
                .await;
                Ok(if let Some(reservation) = reservation {
                    super::idempotency::record(response, reservation, 8388608)
                } else {
                    response
                })
            }
            ("POST", ["feedback"]) => {
                let s = existing
                    .ok_or_else(|| AgentError::code(404, "AGENT_RUNTIME_SESSION_NOT_FOUND"))?;
                let rating = payload["rating"]
                    .as_i64()
                    .filter(|r| [-1, 1].contains(r))
                    .ok_or_else(|| AgentError::code(422, "INVALID_RATING"))?;
                let row:Value=sqlx::query_scalar("INSERT INTO agent_runtime_feedback AS f(tenant_id,publication_id,agent_version_id,session_id,principal_id,rating,comment,channel) SELECT $1,$2::uuid,$3::uuid,$4,$5,$6,$7,$8 WHERE EXISTS(SELECT 1 FROM assistant.sessions WHERE session_id=$4 AND tenant_id=$1 AND publication_id=$2::uuid AND agent_version_id=$3::uuid AND user_id=$5 AND channel=$8 AND status='active') ON CONFLICT(tenant_id,publication_id,session_id,principal_id) DO UPDATE SET rating=EXCLUDED.rating,comment=EXCLUDED.comment RETURNING to_jsonb(f)").bind(&scope.tenant_id).bind(text(&s,"publication_id")).bind(text(&s,"agent_version_id")).bind(&session).bind(&scope.user_id).bind(rating).bind(text(&payload,"comment")).bind(channel).fetch_optional(&state.repository.pool).await?.ok_or_else(||AgentError::code(404,"AGENT_RUNTIME_SESSION_NOT_FOUND"))?;
                Ok(Json(json!({"request_id":req_id,"feedback_id":row["feedback_id"],"session_id":row["session_id"],"rating":row["rating"]})).into_response())
            }
            _ => Err(AgentError::code(404, "NOT_FOUND")),
        }
    }
    async fn tokens_route(
        &self,
        state: &AgentState,
        headers: &HeaderMap,
        rest: &str,
        method: &str,
        payload: Value,
    ) -> Result<Response, AgentError> {
        let actor = Actor::resolve(
            &state.auth,
            headers,
            &format!("/api/v1/publications/{rest}"),
        )
        .await?;
        if method != "GET" && !state.management_enabled {
            return Err(AgentError::code(503, "AGENT_STUDIO_MUTATIONS_DISABLED"));
        }
        let parts: Vec<_> = rest.split('/').collect();
        let publication = super::uuid(parts[0])?;
        if parts.get(1) == Some(&"rollback") && parts.len() == 2 && method == "POST" {
            if !state.publish_enabled {
                return Err(AgentError::code(503, "AGENT_STUDIO_PUBLISH_DISABLED"));
            }
            let key = headers
                .get("idempotency-key")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AgentError::code(400, "AGENT_RELEASE_IDEMPOTENCY_KEY_REQUIRED"))?;
            super::routes::closed(&payload, &["target_version_id", "reason"])?;
            super::routes::bounded(&payload, "reason", 1, 1000, true)?;
            let mut value = state
                .rollback_publication(&actor, &publication, key, &payload)
                .await?;
            value["request_id"] = headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .into();
            return Ok(Json(value).into_response());
        }
        if parts.get(1) != Some(&"tokens") {
            return Err(AgentError::code(404, "NOT_FOUND"));
        }
        let mut tx = state.repository.pool.begin().await?;
        let owner:Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_publications p JOIN agent_members m ON m.tenant_id=p.tenant_id AND m.agent_id=p.agent_id WHERE p.tenant_id=$1 AND p.publication_id=$2::uuid AND m.principal_type='user' AND m.principal_id=$3 AND m.role='owner'").bind(&actor.tenant_id).bind(&publication).bind(&actor.user_id).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(404,"AGENT_PUBLICATION_NOT_FOUND"))?;
        if method == "GET" && parts.len() == 2 {
            let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(t)-'token_hash' FROM agent_api_tokens t WHERE tenant_id=$1 AND publication_id=$2::uuid ORDER BY created_at DESC").bind(&actor.tenant_id).bind(&publication).fetch_all(&mut *tx).await?;
            return Ok(Json(json!({"tokens":rows,"request_id":headers.get("x-request-id").and_then(|v|v.to_str().ok()).unwrap_or("")})).into_response());
        }
        if method == "DELETE" && parts.len() == 3 {
            let token = super::uuid(parts[2])?;
            let row:Value=sqlx::query_scalar("UPDATE agent_api_tokens t SET revoked_at=COALESCE(revoked_at,NOW()) WHERE tenant_id=$1 AND publication_id=$2::uuid AND token_id=$3::uuid RETURNING to_jsonb(t)-'token_hash'").bind(&actor.tenant_id).bind(&publication).bind(&token).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(404,"AGENT_API_TOKEN_NOT_FOUND"))?;
            super::studio::audit(
                &mut tx,
                &actor,
                text(&owner, "agent_id"),
                "api_token_revoke",
                json!({"publication_id":publication,"token_id":token,"channel":owner["channel"]}),
            )
            .await?;
            tx.commit().await?;
            return Ok(Json(row).into_response());
        }
        if method != "POST" {
            return Err(AgentError::code(405, "METHOD_NOT_ALLOWED"));
        }
        super::routes::closed(&payload, &["name", "scopes", "expires_at"])?;
        if let Some(scopes) = payload.get("scopes").filter(|v| !v.is_null()) {
            if !scopes
                .as_array()
                .is_some_and(|a| !a.is_empty() && a.len() <= 4 && a.iter().all(Value::is_string))
            {
                return Err(AgentError::code(422, "AGENT_RUNTIME_TOKEN_SCOPE_INVALID"));
            }
        }
        let old = if parts.len() == 4 && parts[3] == "rotate" {
            let old:Value=sqlx::query_scalar("SELECT to_jsonb(t) FROM agent_api_tokens t WHERE tenant_id=$1 AND publication_id=$2::uuid AND token_id=$3::uuid FOR UPDATE").bind(&actor.tenant_id).bind(&publication).bind(super::uuid(parts[2])?).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(404,"AGENT_API_TOKEN_NOT_FOUND"))?;
            if !old["revoked_at"].is_null() {
                return Err(AgentError::code(404, "AGENT_API_TOKEN_NOT_FOUND"));
            }
            Some(old)
        } else if parts.len() == 2 {
            None
        } else {
            return Err(AgentError::code(404, "NOT_FOUND"));
        };
        if old.is_none() && (owner["channel"] != "api" || owner["status"] != "active") {
            return Err(AgentError::code(409, "PUBLICATION_CHANNEL_MISMATCH"));
        }
        let scopes = payload
            .get("scopes")
            .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
            .or_else(|| old.as_ref().map(|v| &v["scopes"]))
            .cloned()
            .unwrap_or(json!([]));
        let scopes: BTreeSet<String> = scopes
            .as_array()
            .ok_or_else(|| AgentError::code(422, "AGENT_RUNTIME_TOKEN_SCOPE_INVALID"))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if scopes.is_empty()
            || scopes.iter().any(|s| {
                ![
                    "chat:write",
                    "sessions:write",
                    "attachments:write",
                    "feedback:write",
                ]
                .contains(&s.as_str())
            })
        {
            return Err(AgentError::code(422, "AGENT_RUNTIME_TOKEN_SCOPE_INVALID"));
        }
        let expiry = payload["expires_at"]
            .as_str()
            .or_else(|| old.as_ref().and_then(|v| v["expires_at"].as_str()))
            .map(|s| DateTime::parse_from_rfc3339(s).map(|d| d.with_timezone(&Utc)))
            .transpose()
            .map_err(|_| AgentError::code(422, "AGENT_RUNTIME_TOKEN_EXPIRY_INVALID"))?;
        if expiry.is_some_and(|v| v <= Utc::now()) {
            return Err(AgentError::code(422, "AGENT_RUNTIME_TOKEN_EXPIRY_INVALID"));
        }
        let name = payload["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| old.as_ref().and_then(|v| v["name"].as_str()))
            .unwrap_or("");
        if name.is_empty() || name.chars().count() > 255 {
            return Err(AgentError::code(422, "AGENT_RUNTIME_TOKEN_NAME_INVALID"));
        }
        let raw = random_token("agt_");
        if let Some(old) = &old {
            sqlx::query("UPDATE agent_api_tokens SET revoked_at=NOW() WHERE tenant_id=$1 AND token_id=$2::uuid").bind(&actor.tenant_id).bind(text(old,"token_id")).execute(&mut *tx).await?;
        }
        let row:Value=sqlx::query_scalar("INSERT INTO agent_api_tokens AS t(tenant_id,publication_id,token_hash,name,scopes,expires_at,created_by,rotated_from_token_id) VALUES($1,$2::uuid,$3,$4,$5,$6,$7,$8::uuid) RETURNING to_jsonb(t)-'token_hash'").bind(&actor.tenant_id).bind(&publication).bind(digest(&raw)).bind(name).bind(scopes.iter().cloned().collect::<Vec<_>>()).bind(expiry).bind(&actor.user_id).bind(old.as_ref().and_then(|v|v["token_id"].as_str())).fetch_one(&mut *tx).await?;
        super::studio::audit(&mut tx,&actor,text(&owner,"agent_id"),if old.is_some(){"api_token_rotate"}else{"api_token_create"},json!({"publication_id":publication,"token_id":row["token_id"],"old_token_id":old.as_ref().map(|v|&v["token_id"]),"name":name,"scopes":scopes,"expires_at":expiry,"channel":owner["channel"]})).await?;
        tx.commit().await?;
        Ok((
            StatusCode::CREATED,
            Json(json!({"token":raw,"token_metadata":row,"request_id":uuid::Uuid::new_v4().to_string()})),
        )
            .into_response())
    }
}
#[derive(Clone)]
pub struct RuntimeIdempotency {
    pub tenant: String,
    pub publication: String,
    pub principal: String,
    pub key: String,
    pub request_hash: String,
}
impl RuntimeIdempotency {
    pub async fn reserve(&self, pool: &sqlx::PgPool, session: &str) -> Result<Value, AgentError> {
        let mut tx = pool.begin().await?;
        let created:Option<Value>=sqlx::query_scalar("INSERT INTO agent_runtime_idempotency AS i(tenant_id,publication_id,principal_id,idempotency_key,request_hash,session_id) VALUES($1,$2::uuid,$3,$4,$5,$6) ON CONFLICT(tenant_id,publication_id,principal_id,idempotency_key) DO UPDATE SET request_hash=EXCLUDED.request_hash,session_id=EXCLUDED.session_id,status='pending',response_body=NULL,response_media_type=NULL,response_status_code=NULL,completed_at=NULL,created_at=NOW(),expires_at=NOW()+INTERVAL '24 hours' WHERE i.expires_at<=NOW() RETURNING jsonb_build_object('created',TRUE,'session_id',i.session_id,'status','pending')").bind(&self.tenant).bind(&self.publication).bind(&self.principal).bind(&self.key).bind(&self.request_hash).bind(session).fetch_optional(&mut *tx).await?;
        if let Some(row) = created {
            tx.commit().await?;
            return Ok(row);
        }
        let row:Value=sqlx::query_scalar("SELECT jsonb_build_object('created',FALSE,'request_hash',request_hash,'session_id',session_id,'status',status,'response_body',encode(response_body,'base64'),'response_media_type',response_media_type,'response_status_code',response_status_code) FROM agent_runtime_idempotency WHERE tenant_id=$1 AND publication_id=$2::uuid AND principal_id=$3 AND idempotency_key=$4 AND expires_at>NOW()").bind(&self.tenant).bind(&self.publication).bind(&self.principal).bind(&self.key).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(409,"AGENT_RUNTIME_IDEMPOTENCY_CONFLICT"))?;
        if row["request_hash"] != self.request_hash {
            return Err(AgentError::code(409, "AGENT_RUNTIME_IDEMPOTENCY_CONFLICT"));
        }
        tx.commit().await?;
        Ok(row)
    }
    pub async fn complete(
        &self,
        pool: &sqlx::PgPool,
        body: Vec<u8>,
        media: &str,
        status: i32,
    ) -> Result<(), AgentError> {
        let result=sqlx::query("UPDATE agent_runtime_idempotency SET status='completed',response_body=$6,response_media_type=$7,response_status_code=$8,completed_at=NOW() WHERE tenant_id=$1 AND publication_id=$2::uuid AND principal_id=$3 AND idempotency_key=$4 AND request_hash=$5 AND status='pending' AND expires_at>NOW()").bind(&self.tenant).bind(&self.publication).bind(&self.principal).bind(&self.key).bind(&self.request_hash).bind(body).bind(media.chars().take(255).collect::<String>()).bind(status).execute(pool).await?;
        if result.rows_affected() != 1 {
            return Err(AgentError::code(
                409,
                "AGENT_RUNTIME_IDEMPOTENCY_STATE_INVALID",
            ));
        }
        Ok(())
    }
    pub async fn fail(&self, pool: &sqlx::PgPool) -> Result<(), AgentError> {
        sqlx::query("UPDATE agent_runtime_idempotency SET status='failed',completed_at=NOW() WHERE tenant_id=$1 AND publication_id=$2::uuid AND principal_id=$3 AND idempotency_key=$4 AND request_hash=$5 AND status='pending'").bind(&self.tenant).bind(&self.publication).bind(&self.principal).bind(&self.key).bind(&self.request_hash).execute(pool).await?;
        Ok(())
    }
}
pub fn reject_forgery(headers: &HeaderMap, body: Option<&Value>) -> Result<(), AgentError> {
    if headers.keys().any(|name| {
        name.as_str().starts_with("x-agent-")
            && !matches!(
                name.as_str(),
                "x-agent-embed-token" | "x-agent-embed-origin"
            )
    }) {
        return Err(AgentError::code(400, "AGENT_RUNTIME_FIELD_FORBIDDEN"));
    }
    if let Some(body) = body {
        let mut fields: Vec<_> = [
            "agent_id",
            "agent_version_id",
            "draft_revision",
            "publication_id",
            "channel",
            "resolved_snapshot",
            "runtime_envelope",
            "snapshot_hash",
            "spec_hash",
            "runtime_fingerprint",
        ]
        .into_iter()
        .filter(|k| body.get(*k).is_some())
        .collect();
        fields.sort();
        if !fields.is_empty() {
            return Err(AgentError {
                status: 422,
                detail: json!({"code":"AGENT_RUNTIME_FIELD_FORBIDDEN","fields":fields}),
            });
        }
    }
    Ok(())
}
