//! Test-only composition using production constructors. No background service starts.
//! The caller must supply isolated PostgreSQL and explicitly opt into loopback Redis.
use super::*;
use ai_platform_auth::{
    PasswordEngine, TokenCodec, accounts::PgAccounts, admission::AdmissionService,
    service::AuthService, sessions::RedisSessions,
};
use ai_platform_gateway::{
    migration_agent::{
        Scope,
        control::{
            RuntimeCatalog, RuntimeControlPlane, RuntimeModels, RuntimeOptions, RuntimeProviders,
        },
        entry::{PgSourceAccess, SourceAuthority, SourceRefs},
        entry_request::AgentRateLimit,
        model::LeaseSigner,
        public::{PublicIdentityResolver, PublicRuntime},
        resources::{RuntimeKnowledge, RuntimeResources},
        routes::{AgentAttachments, AgentState},
    },
    migration_trace::TraceRepository,
    session_manager::{MemorySessionManager, SessionManagerConfig},
};
use axum::http::{HeaderMap, HeaderName};
use sqlx::postgres::PgPoolOptions;
use std::sync::atomic::{AtomicBool, Ordering};
pub struct TestModels {
    pub pool: PgPool,
}
impl RuntimeModels for TestModels {
    fn model<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            sqlx::query_scalar("SELECT to_jsonb(m) FROM llm_models m WHERE tenant_id=$1 AND model_id=$2 AND($3::text IS NULL OR provider_id=$3)").bind(tenant).bind(model).bind(provider).fetch_optional(&self.pool).await?.ok_or_else(||AgentError::code(503,"AGENT_RUNTIME_MODEL_UNAVAILABLE"))
        })
    }
}
pub struct TestProviders {
    pub pool: PgPool,
    pub configured: AtomicBool,
}
impl RuntimeProviders for TestProviders {
    fn provider<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            sqlx::query_scalar(
                "SELECT to_jsonb(p) FROM llm_providers p WHERE tenant_id=$1 AND provider_id=$2",
            )
            .bind(tenant)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AgentError::code(503, "AGENT_RUNTIME_PROVIDER_UNAVAILABLE"))
        })
    }
    fn runtime_config<'a>(&'a self, _: &'a str, _: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            if !self.configured.load(Ordering::SeqCst) {
                return Err(AgentError::code(503, "AGENT_RUNTIME_PROVIDER_UNAVAILABLE"));
            }
            Ok(
                json!({"provider_type":"openai","base_url":"http://127.0.0.1:1","api_key":"synthetic-unused-fixture"}),
            )
        })
    }
}
struct Collaborators;
impl RuntimeCatalog for Collaborators {
    fn fetch<'a>(
        &'a self,
        _: &'a Scope,
        _: &'a str,
        _: i64,
        _: Option<&'a [Value]>,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async { Ok(json!({"tools":[]})) })
    }
}
impl SourceAuthority for Collaborators {
    fn allowed<'a>(&'a self, _: &'a Scope, refs: &'a SourceRefs) -> AgentFuture<'a, bool> {
        Box::pin(async move {
            Ok(refs.datasets.is_empty() && refs.documents.is_empty() && refs.versions.is_empty())
        })
    }
}
impl RuntimeResources for Collaborators {
    fn effective<'a>(
        &'a self,
        _: &'a Actor,
        bindings: &'a [Value],
        _: &'a str,
        _: &'a Value,
    ) -> AgentFuture<'a, Vec<Value>> {
        Box::pin(async move { Ok(bindings.to_vec()) })
    }
}
impl RuntimeKnowledge for Collaborators {
    fn effective<'a>(
        &'a self,
        _: &'a Actor,
        bindings: &'a [Value],
        _: &'a str,
        _: &'a str,
    ) -> AgentFuture<'a, BTreeSet<String>> {
        Box::pin(async move {
            Ok(bindings
                .iter()
                .filter_map(|x| x["dataset_id"].as_str().map(str::to_owned))
                .collect())
        })
    }
}
impl AgentAttachments for Collaborators {
    fn bind<'a>(
        &'a self,
        _: &'a Actor,
        _: &'a Scope,
        _: &'a str,
        _: Option<&'a str>,
        refs: &'a [String],
    ) -> AgentFuture<'a, (Vec<String>, Vec<String>)> {
        Box::pin(async move {
            if refs.is_empty() {
                Ok((vec![], vec![]))
            } else {
                Err(AgentError::code(403, "ATTACHMENT_FORBIDDEN"))
            }
        })
    }
}
impl AgentRateLimit for Collaborators {
    fn check<'a>(&'a self, _: &'a Actor, _: &'a str) -> AgentFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
struct Identities;
impl PublicIdentityResolver for Identities {
    fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> AgentFuture<'a, Actor> {
        Box::pin(async {
            Ok(Actor {
                user_id: "guest".into(),
                tenant_id: "public".into(),
                tier: "anonymous".into(),
                roles: vec!["guest".into()],
            })
        })
    }
}
pub struct TestState {
    pub state: AgentState,
    pub providers: Arc<TestProviders>,
    pub control_pool: PgPool,
}
pub async fn state(pool: PgPool) -> Result<TestState> {
    let redis_url = std::env::var("AI_PLATFORM_STUDIO_TEST_REDIS_URL").map_err(
        |_| "AI_PLATFORM_STUDIO_TEST_REDIS_URL must explicitly select isolated loopback Redis",
    )?;
    let url = reqwest::Url::parse(&redis_url)?;
    if url.scheme() != "redis"
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
    {
        return Err("test Redis must be loopback".into());
    }
    let redis = redis::Client::open(redis_url)
        .map_err(|_| "invalid isolated Redis URL")?
        .get_connection_manager()
        .await
        .map_err(|_| "isolated Redis unavailable")?;
    let secret = b"studio-fixture-signing-material-only-32-bytes";
    let codec = TokenCodec::new(secret, None, None)?;
    let auth = Arc::new(AdmissionService::new(
        Arc::new(AuthService::new(
            PgAccounts::new(pool.clone()),
            RedisSessions::new(redis.clone()),
            codec,
            PasswordEngine::new(1)?,
            "fixture.invalid".into(),
        )),
        pool.clone(),
        HeaderName::from_static("x-api-key"),
    )?);
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&pool)
        .await?;
    let control_pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |conn, _| {
            let schema = schema.clone();
            Box::pin(async move {
                sqlx::query("SELECT set_config('search_path',quote_ident($1)||',public',false)")
                    .bind(schema)
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_lazy_with(pool.connect_options().as_ref().clone());
    let providers = Arc::new(TestProviders {
        pool: pool.clone(),
        configured: AtomicBool::new(true),
    });
    let collaborators = Arc::new(Collaborators);
    let sources = Arc::new(PgSourceAccess {
        pool: pool.clone(),
        authority: collaborators.clone(),
    });
    let control = Arc::new(RuntimeControlPlane::new(
        pool.clone(),
        Arc::new(TestModels { pool: pool.clone() }),
        providers.clone(),
        collaborators.clone(),
        sources.clone(),
        Arc::new(LeaseSigner::new("synthetic-test-lease-secret".into())?),
        RuntimeOptions {
            runtime_url: "http://127.0.0.1:1".into(),
            runtime_internal_token: "synthetic-internal-fixture".into(),
            model_plane_base_url: "http://127.0.0.1:1".into(),
            kernel_revision: "fixture-kernel".into(),
            lease_ttl_seconds: 300,
            max_model_calls: 4,
            max_cost_microusd: 1_000_000,
            worker_writes_enabled: false,
        },
    )?);
    let state = AgentState {
        auth,
        repository: Arc::new(AgentRepository::new(
            pool.clone(),
            Arc::new(Resources { pool: pool.clone() }),
        )),
        control,
        sessions: Arc::new(MemorySessionManager::new(SessionManagerConfig::default())?),
        sources,
        attachments: collaborators.clone(),
        traces: Arc::new(TraceRepository::new(pool, control_pool.clone())?),
        edge_rate: collaborators.clone(),
        cleanup: None,
        runtime_resources: collaborators.clone(),
        runtime_knowledge: collaborators,
        default_model: "qwen3.7-plus".into(),
        management_enabled: true,
        publish_enabled: true,
        runtime_enabled: true,
        public: Some(Arc::new(PublicRuntime {
            identities: Arc::new(Identities),
            redis,
            embed_secret: Arc::new(String::from_utf8(secret.to_vec())?),
            storage: None,
            max_attachment_bytes: 16 * 1024 * 1024,
        })),
    };
    Ok(TestState {
        state,
        providers,
        control_pool,
    })
}
/// Preserve cleanup on test errors and assertion panics, including the independent control pool.
pub async fn fixture<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(PgPool, TestState) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    super::fixture(|pool, _| async move {
        let graph = state(pool.clone()).await?;
        let control = graph.control_pool.clone();
        let outcome = std::panic::AssertUnwindSafe(test(pool, graph))
            .catch_unwind()
            .await;
        control.close().await;
        match outcome {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
    .await
}
/// A real native JWT and Redis allowlist entry for an existing isolated fixture actor.
/// The caller must revoke the returned JTI even when an assertion panics.
pub async fn authenticate_fixture(graph: &TestState, actor: &Actor) -> Result<(String, String)> {
    let codec = TokenCodec::new(b"studio-fixture-signing-material-only-32-bytes", None, None)?;
    let now = chrono::Utc::now().timestamp() as u64;
    let token = codec.issue(
        ai_platform_auth::Claims {
            sub: actor.user_id.clone(),
            user_id: actor.user_id.clone(),
            tenant_id: actor.tenant_id.clone(),
            email: None,
            roles: actor.roles.clone(),
            permissions: vec![],
            tier: actor.tier.clone(),
            exp: 0,
            iat: 0,
            nbf: None,
            jti: String::new(),
            aud: None,
            iss: None,
        },
        now,
    )?;
    let issued = codec.verify(&token, now)?;
    RedisSessions::new(
        graph
            .state
            .public
            .as_ref()
            .ok_or("public runtime absent")?
            .redis
            .clone(),
    )
    .save(
        &issued.jti,
        &actor.user_id,
        &json!({"user_id":actor.user_id,"credential_revision":null}),
        300,
    )
    .await?;
    Ok((token, issued.jti))
}
pub async fn revoke_fixture(graph: &TestState, actor: &Actor, jti: &str) -> Result<()> {
    RedisSessions::new(
        graph
            .state
            .public
            .as_ref()
            .ok_or("public runtime absent")?
            .redis
            .clone(),
    )
    .revoke(jti, &actor.user_id)
    .await?;
    Ok(())
}
