//! Concrete bindings to the canonical identity, rate, model and source authorities.
use super::{
    ContentAccess, ContentError, ContentFuture, ContentIdentity,
    artifacts::Artifact,
    shares::{ShareRuntimeHistory, ShareViewer},
};
use crate::{
    client_ip::TrustedProxies,
    migration_agent::{
        AgentError, Scope,
        control::RuntimeModels,
        entry::{PgSourceAccess, SourceAccess},
        threads::AgentThreadStore,
    },
    quiz_rate,
    rate_limits::{MultiDimensionConfig, RateLimitContext, SlidingWindowRateLimiter},
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, permission_denied_detail, require_capability},
};
use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
#[derive(Clone, Default)]
pub struct ContentRequestContext {
    pub path: String,
    pub client_ip: String,
    pub counted_dimensions: BTreeSet<String>,
    pub trace_id: String,
    pub anonymous_id: Option<String>,
    identity: Arc<tokio::sync::OnceCell<Result<ContentIdentity, ContentError>>>,
}
tokio::task_local! {static REQUEST:ContentRequestContext;}
pub(crate) fn context() -> ContentRequestContext {
    REQUEST.try_with(Clone::clone).unwrap_or_default()
}
/// Mount once around the merged public content routers. Auth resolution is
/// request-cached; peer IP and counted dimensions come only from trusted extensions.
pub async fn request_context(
    State(access): State<Arc<NativeContentAccess>>,
    mut request: Request,
    next: Next,
) -> Response {
    let (ip, mut counted) = quiz_rate::request_context(
        request.headers(),
        request.extensions(),
        &access.trusted_proxies,
    );
    if let Some(charged) = request
        .extensions()
        .get::<crate::migration_host::rate::CountedDimensions>()
    {
        counted.extend(charged.0.iter().cloned());
    }
    let host = request
        .extensions()
        .get::<crate::migration_host::middleware::RequestContext>()
        .cloned();
    let mut viewer = request
        .extensions()
        .get::<ShareViewer>()
        .cloned()
        .unwrap_or_default();
    viewer.client_ip = ip.clone();
    if let Some(host) = &host {
        viewer.anonymous_id = host.anonymous_id.clone();
    }
    request.extensions_mut().insert(viewer.clone());
    request.extensions_mut().insert(quiz_rate::QuizRequestContext {
        direct_client_host: Some(ip.clone()), counted_dimensions: counted.clone(),
    });
    let ctx = ContentRequestContext {
        path: request.uri().path().into(),
        client_ip: ip,
        counted_dimensions: counted,
        trace_id: host
            .as_ref()
            .map(|c| c.trace_id.clone())
            .unwrap_or_default(),
        anonymous_id: viewer.anonymous_id,
        ..Default::default()
    };
    REQUEST.scope(ctx, next.run(request)).await
}
impl From<AgentError> for ContentError {
    fn from(e: AgentError) -> Self {
        Self::new(e.status, e.detail)
    }
}
fn scope(user: &ContentIdentity, session: &str) -> Scope {
    Scope {
        tenant_id: user.tenant_id.clone(),
        user_id: user.user_id.clone(),
        session_id: session.into(),
    }
}
#[derive(Clone)]
pub struct NativeContentAccess {
    pub identities: Arc<EndpointIdentityResolver>,
    pub rbac: Arc<Rbac>,
    pub threads: AgentThreadStore,
    pub sources: Arc<PgSourceAccess>,
    pub models: Arc<dyn RuntimeModels>,
    pub limiter: Option<Arc<SlidingWindowRateLimiter>>,
    pub rate_config: Arc<MultiDimensionConfig>,
    pub trusted_proxies: TrustedProxies,
}
impl NativeContentAccess {
    async fn check_rate(
        &self,
        user: Option<&ContentIdentity>,
        operation: &str,
        assistant: Option<&str>,
    ) -> Result<(), ContentError> {
        let Some(limiter) = &self.limiter else {
            return Ok(());
        };
        let ctx = context();
        let actor = RateLimitContext {
            ip: ctx.client_ip,
            user_id: user.map(|u| u.user_id.clone()).filter(|s| !s.is_empty()),
            user_tier: user
                .map(|u| u.tier.clone())
                .unwrap_or_else(|| "anonymous".into()),
            tenant_id: user.map(|u| u.tenant_id.clone()).filter(|s| !s.is_empty()),
            assistant_id: assistant.map(str::to_owned),
            operation: Some(operation.into()),
        };
        let skip = if assistant.is_some() {
            BTreeSet::new()
        } else {
            ctx.counted_dimensions
        };
        let policies = self
            .rate_config
            .policies(&actor, &skip)
            .map_err(|_| ContentError::new(503, "Rate limiter unavailable"))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| ContentError::new(503, "Rate limiter unavailable"))?
            .as_secs_f64();
        let result = limiter
            .check_dimensions_at(&policies, now)
            .await
            .map_err(|_| ContentError::new(503, "Rate limiter unavailable"))?;
        if result.allowed {
            return Ok(());
        }
        let mut error = ContentError::new(
            429,
            if assistant.is_some() {
                result.exceeded_response()
            } else {
                json!(format!("Rate limit exceeded ({operation})"))
            },
        );
        error.headers = if assistant.is_some() {
            result.headers()
        } else {
            std::collections::BTreeMap::from([
                ("X-RateLimit-Limit".into(), result.limit.to_string()),
                ("X-RateLimit-Remaining".into(), result.remaining.to_string()),
                ("Retry-After".into(), result.retry_after.to_string()),
            ])
        };
        Err(error)
    }
}
impl ContentAccess for NativeContentAccess {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> ContentFuture<'a, ContentIdentity> {
        Box::pin(async move {
            let ctx = context();
            ctx.identity
                .get_or_init(|| {
                    self.identities.resolve_request(
                        headers,
                        &ctx.path,
                        &ctx.client_ip,
                        ctx.anonymous_id.as_deref(),
                    )
                })
                .await
                .clone()
        })
    }
    fn require_capability<'a>(
        &'a self,
        user: &'a ContentIdentity,
        name: &'a str,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let capability: Capability = serde_json::from_value(json!(name))
                .map_err(|_| ContentError::new(403, "Unknown capability"))?;
            require_capability(&self.rbac, &user.roles, &user.permissions, capability).map_err(
                |_| {
                    ContentError::new(
                        403,
                        permission_denied_detail(capability, &context().trace_id, None),
                    )
                },
            )?;
            Ok(())
        })
    }
    fn rate_limit<'a>(
        &'a self,
        user: &'a ContentIdentity,
        operation: &'a str,
        service: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(self.check_rate(Some(user), operation, service))
    }
    fn authorize_session<'a>(
        &'a self,
        user: &'a ContentIdentity,
        session: &'a str,
    ) -> ContentFuture<'a, Value> {
        Box::pin(async move {
            let scope = scope(user, session);
            let owned = self.threads.owned_session(&scope).await.map_err(|e| {
                if e.status == 404 {
                    ContentError::new(404, "Session not found")
                } else {
                    ContentError::new(500, "Failed to verify session")
                }
            })?;
            Ok(owned)
        })
    }
    fn authorize_artifact_source<'a>(
        &'a self,
        user: &'a ContentIdentity,
        artifact: &'a Artifact,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            self.sources
                .require_artifact(
                    &scope(user, &artifact.session_id),
                    &serde_json::to_value(artifact)?,
                )
                .await?;
            Ok(())
        })
    }
    fn require_vision_model<'a>(
        &'a self,
        user: &'a ContentIdentity,
        id: &'a str,
        provider: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async move {
            let model = self
                .models
                .model(&user.tenant_id, id, provider)
                .await
                .map_err(|_| {
                    ContentError::new(503, json!({"code":"MODEL_CAPABILITY_UNAVAILABLE"}))
                })?;
            if model["is_enabled"] != true || model["supports_vision"] != true {
                return Err(ContentError::new(
                    409,
                    json!({"code":"VISION_MODEL_REQUIRED"}),
                ));
            }
            Ok(())
        })
    }
}
pub struct NativeShareHistory {
    pub access: Arc<NativeContentAccess>,
}
impl ShareRuntimeHistory for NativeShareHistory {
    fn history<'a>(
        &'a self,
        user: &'a ContentIdentity,
        session: &'a str,
        limit: usize,
    ) -> ContentFuture<'a, Option<(Vec<Value>, usize)>> {
        Box::pin(async move {
            Ok(self
                .access
                .threads
                .history_messages(&scope(user, session), limit, &self.access.sources)
                .await?)
        })
    }
    fn public_submit_rate_limit<'a>(&'a self, _headers: &'a HeaderMap) -> ContentFuture<'a, ()> {
        Box::pin(self.access.check_rate(None, "quiz_submit_public", None))
    }
}

/// get_user_context's guest/anonymous branch around the accepted current-account
/// admission authority. Invalid presented credentials never become anonymous.
pub struct EndpointIdentityResolver {
    pub admission: Arc<AdmissionService>,
    pub jwt_enabled: bool,
    pub api_key_enabled: bool,
    pub api_key_header: axum::http::HeaderName,
    pub static_keys: Option<StaticKeyFallback>,
}
/// Explicit source static-key mode. Hashes are derived from configured keys by
/// host setup; absent active DB key may fall back, a rejected active DB key may not.
pub struct StaticKeyFallback {
    pub database: Option<super::ContentDatabase>,
    pub key_hashes: Vec<String>,
    pub role: String,
}
impl EndpointIdentityResolver {
    pub async fn resolve_request(
        &self,
        headers: &HeaderMap,
        path: &str,
        client_ip: &str,
        anonymous_id: Option<&str>,
    ) -> Result<ContentIdentity, ContentError> {
        let guest = |user: String| ContentIdentity {
            user_id: user,
            tenant_id: "public".into(),
            tier: "anonymous".into(),
            roles: vec!["guest".into()],
            is_authenticated: false,
            ..Default::default()
        };
        if !self.jwt_enabled && !self.api_key_enabled {
            return Ok(guest("guest".into()));
        }
        let bearer = self.jwt_enabled
            && headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|s| s.to_ascii_lowercase().starts_with("bearer "));
        let key = self.api_key_enabled
            && headers
                .get(&self.api_key_header)
                .is_some_and(|v| !v.is_empty());
        if !bearer && !key {
            return Ok(guest(format!(
                "anon:{}",
                anonymous_id.filter(|s| !s.is_empty()).unwrap_or(client_ip)
            )));
        }
        if !bearer {
            if let Some(fallback) = &self.static_keys {
                let raw = headers
                    .get(&self.api_key_header)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| ContentError::new(401, "Invalid API key"))?;
                let hash = super::sha256(raw.as_bytes());
                let active = if let Some(db) = &fallback.database {
                    let h = hash.clone();
                    db.run(move|c|Box::pin(async move{sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM public.api_keys WHERE key_hash=$1 AND enabled=TRUE AND (expires_at IS NULL OR expires_at>NOW()))").bind(h).fetch_one(c).await})).await?
                } else {
                    false
                };
                if !active {
                    let matched = fallback.key_hashes.iter().fold(false, |matched, allowed| {
                        super::capabilities::constant_time_equal(&hash, allowed) | matched
                    });
                    if matched {
                        let role = fallback.role.trim();
                        let role = if ["user", "admin", "operator", "developer"].contains(&role) {
                            role
                        } else {
                            "user"
                        };
                        return Ok(ContentIdentity {
                            user_id: format!("apikey:{}", &hash[..16]),
                            tenant_id: String::new(),
                            roles: vec![role.into()],
                            tier: if role == "admin" { "admin" } else { "normal" }.into(),
                            is_authenticated: true,
                            permissions: vec![],
                        });
                    }
                }
            }
        }
        let mut selected = headers.clone();
        if !bearer {
            selected.remove(axum::http::header::AUTHORIZATION);
        }
        if !self.api_key_enabled {
            selected.remove(&self.api_key_header);
        }
        let request = self.admission.request(&selected, path);
        let principal = request
            .resolve()
            .await
            .map_err(|e| ContentError::new(e.status(), "Authentication required"))?;
        Ok(ContentIdentity {
            tenant_id: principal.tenant_id().into(),
            user_id: principal.user_id().into(),
            roles: principal.roles().to_vec(),
            permissions: principal.permissions().to_vec(),
            tier: principal.tier().into(),
            is_authenticated: true,
        })
    }
}
impl crate::migration_agent::public::PublicIdentityResolver for EndpointIdentityResolver {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> crate::migration_agent::AgentFuture<'a, crate::migration_agent::Actor> {
        Box::pin(async move {
            let ctx = context();
            let user = self
                .resolve_request(headers, path, &ctx.client_ip, ctx.anonymous_id.as_deref())
                .await
                .map_err(|e| AgentError {
                    status: e.status,
                    detail: e.detail,
                })?;
            Ok(crate::migration_agent::Actor {
                tenant_id: user.tenant_id,
                user_id: user.user_id,
                roles: user.roles,
                tier: user.tier,
            })
        })
    }
}
