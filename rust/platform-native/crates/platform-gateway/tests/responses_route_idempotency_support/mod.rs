//! Opt-in route fixture for the production Responses handler.
//!
//! It clones the explicitly selected, owner-verified native epoch-24 database
//! into one disposable loopback database. The production auth/session/control
//! state uses scoped Gateway credentials; only the Runtime HTTP boundary is
//! stubbed, with its database effect executed through the scoped Runtime role.
//! No repository .env file, provider credential, or live Runtime is consulted.

use ai_platform_auth::{
    Claims, PasswordEngine, TokenCodec, accounts::PgAccounts, admission::AdmissionService,
    service::AuthService, sessions::RedisSessions,
};
use ai_platform_gateway::{
    gateway_model_meta::MetadataModels,
    migration_agent::{
        Actor, AgentError, AgentFuture,
        control::{
            RuntimeCatalog, RuntimeControlPlane, RuntimeMemory, RuntimeModels, RuntimeOptions,
            RuntimeProviders,
        },
        entry::{PgSourceAccess, SourceAccess, SourceAuthority, SourceRefs},
        model::LeaseSigner,
    },
    migration_compat::responses::{self, ResponsesIdempotency, ResponsesState},
    migration_host::memory::PgRuntimeMemory,
    model_provider_storage::{PgModelDatabase, PgProviderDatabase},
    model_service::{ModelFuture, ModelPricingCache, ModelRow, ModelService, ModelServiceError},
    provider_cipher::NativeProviderCipher,
    provider_probe_http::ReqwestProviderHttp,
    provider_service::{
        GoogleCredentialState, ProviderClock, ProviderEnvironment, ProviderGoogleAuth,
        ProviderService, ServiceError, ServiceFuture,
    },
    rate_limits::MultiDimensionConfig,
    session_manager::{DatabaseSessionConfig, DatabaseSessionManager, SessionStorage},
    session_storage::PgSessionStorage,
    sessions_api::SessionsApiManager,
};
use ai_platform_migration_authority::{
    DEFAULT_BASELINE_ID, bootstrap, commands, credential_bootstrap, credentials,
    runner::{AuthorityPaths, MigrationAuthority},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use chrono::{DateTime, Utc};
use redis::aio::ConnectionManager;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgPool, postgres::PgPoolOptions};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, oneshot},
    task::JoinHandle,
};
use tower::ServiceExt;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

const OPT_IN: &str = "AI_PLATFORM_RESPONSES_ROUTE_TEST";
const TEMPLATE_DSN: &str = "AI_PLATFORM_RESPONSES_ROUTE_TEST_TEMPLATE_DSN";
const GATEWAY_DSN: &str = "AI_PLATFORM_RESPONSES_ROUTE_TEST_GATEWAY_DSN";
const REDIS_URL: &str = "AI_PLATFORM_RESPONSES_ROUTE_TEST_REDIS_URL";
const SIGNING_KEY: &[u8] = b"responses-route-fixture-signing-material-only-32";

type FixtureModels = ModelService<PgModelDatabase, NoopPricingCache>;
type FixtureProviders = ProviderService<
    PgProviderDatabase,
    NativeProviderCipher,
    ReqwestProviderHttp,
    NoGoogleAuth,
    EmptyProviderEnvironment,
    UtcProviderClock,
>;

pub struct ActorFixture {
    pub actor: Actor,
    pub token: String,
    jti: String,
}

pub struct ProjectFixture {
    pub pool: PgPool,
    control_pool: PgPool,
    pub runtime_pool: PgPool,
    admin: PgPool,
    database_name: String,
    pub redis: ConnectionManager,
    pub actor_a: ActorFixture,
    pub actor_b: ActorFixture,
    pub model_id: String,
    pub provider_id: String,
    auth: Arc<AdmissionService>,
    sessions: Arc<DatabaseSessionManager>,
    models: Arc<FixtureModels>,
    providers: Arc<FixtureProviders>,
    memory: Arc<PgRuntimeMemory>,
    signer: Arc<LeaseSigner>,
    cleanup: Mutex<Cleanup>,
}

#[derive(Default)]
struct Cleanup {
    tokens: Vec<(String, String)>,
    idempotency_scopes: Vec<String>,
}

impl ProjectFixture {
    pub async fn new() -> Result<Self> {
        if std::env::var(OPT_IN).as_deref() != Ok("1") {
            return Err(format!("{OPT_IN}=1 is required in addition to --ignored").into());
        }
        let template_dsn = loopback_dsn(TEMPLATE_DSN)?;
        let gateway_dsn = loopback_dsn(GATEWAY_DSN)?;
        let redis_url = loopback_url(REDIS_URL, &["redis"])?;
        let admin_dsn = credentials::dsn_for_principal("admin")?;
        let runtime_dsn = credentials::dsn_for_principal("runtime")?;
        let role_prefix = credentials::role_prefix_from_env()?;
        let authority_paths = repository_authority_paths()?;
        let source_database = database_name(&template_dsn)?;
        validate_source_dsns(
            &template_dsn,
            &gateway_dsn,
            &admin_dsn,
            &runtime_dsn,
            &source_database,
            &role_prefix,
        )?;

        // The native local-target guard verifies that all authority roles point
        // at this source database and that its running PostgreSQL container is
        // owned by this repository's Compose project. Native authority and the
        // Gateway's own admission guard then verify the exact source contract.
        credential_bootstrap::verify_local_target(&authority_paths, &role_prefix, &source_database)
            .await?;
        verify_epoch_24(&template_dsn, &authority_paths, &role_prefix, "template").await?;
        verify_gateway_admission(&gateway_dsn, &authority_paths, &role_prefix).await?;

        // CREATE DATABASE must run from a database other than its template.
        // The admin connection is used only for this fixture database's
        // creation, seed rows, and cleanup; all application state below uses
        // the repository's scoped Gateway/Runtime DSNs.
        let admin_dsn = database_url_for(&admin_dsn, "postgres")?;
        let admin = connect_pool(&admin_dsn, 1).await?;
        let database_name = format!("responses_route_{}", Uuid::new_v4().simple());
        let occupied: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
                .bind(&database_name)
                .fetch_one(&admin)
                .await?;
        if occupied {
            admin.close().await;
            return Err("unique Responses fixture database name is already occupied".into());
        }
        let template_owner: String =
            sqlx::query_scalar("SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=$1")
                .bind(&source_database)
                .fetch_one(&admin)
                .await?;
        if !safe_database_identifier(&template_owner) {
            admin.close().await;
            return Err("template database owner identifier is not supported".into());
        }
        if sqlx::query(&format!(
            "CREATE DATABASE \"{database_name}\" WITH TEMPLATE \"{source_database}\" OWNER \"{template_owner}\""
        ))
            .execute(&admin)
            .await
            .is_err()
        {
            admin.close().await;
            return Err("cannot clone the verified Responses template database".into());
        }

        let setup = async {
            let gateway_test_dsn = database_url_for(&gateway_dsn, &database_name)?;
            let runtime_test_dsn = database_url_for(&runtime_dsn, &database_name)?;
            let template_test_dsn = database_url_for(&template_dsn, &database_name)?;
            let admin_seed_dsn =
                database_url_for(&credentials::dsn_for_principal("admin")?, &database_name)?;
            // PostgreSQL clones object ACLs, but not the database's own ACL.
            // Use the frozen native authority and atomically refuse any change
            // to global role attributes, credentials or membership semantics.
            provision_cloned_database(&admin_seed_dsn, &authority_paths, &role_prefix).await?;
            let pool = connect_pool(&gateway_test_dsn, 4).await?;
            let control_pool = connect_pool(&gateway_test_dsn, 2).await?;
            let runtime_pool = connect_pool(&runtime_test_dsn, 2).await?;

            verify_epoch_24(
                &template_test_dsn,
                &authority_paths,
                &role_prefix,
                "cloned fixture",
            )
            .await?;
            verify_gateway_admission(&gateway_test_dsn, &authority_paths, &role_prefix).await?;

            let seed_pool = connect_pool(&admin_seed_dsn, 1).await?;
            let redis = redis::Client::open(redis_url)
                .map_err(|_| "invalid loopback Redis test URL")?
                .get_connection_manager()
                .await
                .map_err(|_| "loopback Redis test service is unavailable")?;

            let suffix = Uuid::new_v4().simple().to_string();
            let tenant_a = format!("responses-tenant-a-{suffix}");
            let tenant_b = format!("responses-tenant-b-{suffix}");
            let user_a = format!("responses-user-a-{suffix}");
            let user_b = format!("responses-user-b-{suffix}");
            let model_id = "responses-route-fixture-model".to_owned();
            let provider_id = "responses-route-fixture-provider".to_owned();
            seed_accounts_and_model(
                &seed_pool,
                &tenant_a,
                &user_a,
                &tenant_b,
                &user_b,
                &model_id,
                &provider_id,
            )
            .await?;
            seed_pool.close().await;

            let codec = TokenCodec::new(SIGNING_KEY, None, None)?;
            let mut actor_a = actor_fixture(&tenant_a, &user_a);
            let mut actor_b = actor_fixture(&tenant_b, &user_b);
            actor_a.token = issue_token(&codec, &redis, &actor_a.actor).await?;
            actor_b.token = issue_token(&codec, &redis, &actor_b.actor).await?;
            actor_a.jti = codec.verify(&actor_a.token, unix_now()?)?.jti;
            actor_b.jti = codec.verify(&actor_b.token, unix_now()?)?.jti;

            let auth_service = Arc::new(AuthService::new(
                PgAccounts::new(pool.clone()),
                RedisSessions::new(redis.clone()),
                codec,
                PasswordEngine::new(1)?,
                "fixture.invalid".into(),
            ));
            let auth = Arc::new(AdmissionService::new(
                auth_service,
                pool.clone(),
                axum::http::HeaderName::from_static("x-api-key"),
            )?);
            let session_storage: Arc<dyn SessionStorage> =
                Arc::new(PgSessionStorage::new(pool.clone(), control_pool.clone())?);
            let sessions = Arc::new(DatabaseSessionManager::new(
                session_storage,
                DatabaseSessionConfig::default(),
            ));
            let models = Arc::new(ModelService::new(
                PgModelDatabase::new(pool.clone(), control_pool.clone())?,
                NoopPricingCache,
            ));
            let providers = Arc::new(ProviderService::new(
                PgProviderDatabase::new(pool.clone(), control_pool.clone())?,
                NativeProviderCipher::new(),
                ReqwestProviderHttp::new(),
                NoGoogleAuth,
                EmptyProviderEnvironment,
                UtcProviderClock,
                "responses-route-test-only-key-material".into(),
            ));
            let memory = Arc::new(PgRuntimeMemory::new(pool.clone(), control_pool.clone())?);
            let signer = Arc::new(LeaseSigner::new(
                "responses-route-test-lease-signing-material".into(),
            )?);
            let fixture = Self {
                pool,
                control_pool,
                runtime_pool,
                admin: admin.clone(),
                database_name: database_name.clone(),
                redis,
                actor_a,
                actor_b,
                model_id,
                provider_id,
                auth,
                sessions,
                models,
                providers,
                memory,
                signer,
                cleanup: Mutex::new(Cleanup::default()),
            };
            fixture.track_token(&fixture.actor_a.actor.user_id, &fixture.actor_a.jti);
            fixture.track_token(&fixture.actor_b.actor.user_id, &fixture.actor_b.jti);
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(fixture)
        }
        .await;

        match setup {
            Ok(fixture) => Ok(fixture),
            Err(error) => {
                let cleanup = drop_database(&admin, &database_name).await;
                admin.close().await;
                cleanup?;
                Err(error)
            }
        }
    }

    pub fn router(
        &self,
        runtime_url: String,
        idempotency: ResponsesIdempotency,
    ) -> Result<RouteHarness> {
        let event_seen = Arc::new(Notify::new());
        let sources: Arc<dyn SourceAccess> = Arc::new(EventSeenSourceAccess {
            inner: Arc::new(PgSourceAccess::new(
                self.pool.clone(),
                Arc::new(EmptySourceAuthority),
            )),
            event_seen: event_seen.clone(),
        });
        let runtime_models: Arc<dyn RuntimeModels> = self.models.clone();
        let runtime_providers: Arc<dyn RuntimeProviders> = self.providers.clone();
        let mut control = RuntimeControlPlane::new(
            self.pool.clone(),
            runtime_models,
            runtime_providers,
            Arc::new(EmptyCatalog),
            sources,
            self.signer.clone(),
            RuntimeOptions {
                runtime_url,
                runtime_internal_token: "responses-route-test-runtime-token".into(),
                model_plane_base_url: "http://127.0.0.1:1".into(),
                kernel_revision: "responses-route-test-kernel".into(),
                lease_ttl_seconds: 60,
                max_model_calls: 2,
                max_cost_microusd: 100_000,
                worker_writes_enabled: false,
            },
        )?;
        let memory: Arc<dyn RuntimeMemory> = self.memory.clone();
        control.memory = Some(memory);
        let control = Arc::new(control);
        let lookup_count = Arc::new(AtomicUsize::new(0));
        let responses_models: Arc<dyn MetadataModels> = Arc::new(CountedModels {
            inner: self.models.clone(),
            lookup_count: lookup_count.clone(),
        });
        let sessions: Arc<dyn SessionsApiManager> = self.sessions.clone();
        let state = Arc::new(ResponsesState {
            auth: self.auth.clone(),
            sessions,
            control,
            models: responses_models,
            idempotency,
            limiter: None,
            rate_config: MultiDimensionConfig::default(),
        });
        Ok(RouteHarness {
            router: responses::router(state),
            lookup_count,
            event_seen,
        })
    }

    pub fn request_body(&self, input: &str, session_id: &str) -> Value {
        json!({
            "model": self.model_id.clone(),
            "provider_id": self.provider_id.clone(),
            "input": input,
            "stream": false,
            "session_id": session_id,
        })
    }

    pub fn track_idempotency_key(&self, tenant: &str, user: &str, key: &str) {
        let scope = idempotency_scope(tenant, user, key);
        self.cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .idempotency_scopes
            .push(scope);
    }

    fn track_token(&self, user: &str, jti: &str) {
        self.cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tokens
            .push((user.to_owned(), jti.to_owned()));
    }

    pub async fn close(self) -> Result<()> {
        let cleanup = self
            .cleanup
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut cleanup_failed = false;
        let sessions = RedisSessions::new(self.redis.clone());
        for (user, jti) in cleanup.tokens {
            if sessions.revoke(&jti, &user).await.is_err() {
                cleanup_failed = true;
            }
        }
        for scope in cleanup.idempotency_scopes {
            let mut redis = self.redis.clone();
            let result: redis::RedisResult<usize> = redis::cmd("DEL")
                .arg(format!("ai-gateway:responses:idem:lock:{scope}"))
                .arg(format!("ai-gateway:responses:idem:response:{scope}"))
                .query_async(&mut redis)
                .await;
            if result.is_err() {
                cleanup_failed = true;
            }
        }
        self.pool.close().await;
        self.control_pool.close().await;
        self.runtime_pool.close().await;
        if drop_database(&self.admin, &self.database_name)
            .await
            .is_err()
        {
            cleanup_failed = true;
        }
        self.admin.close().await;
        if cleanup_failed {
            Err("one or more exact test-owned Redis/PostgreSQL cleanup operations failed".into())
        } else {
            Ok(())
        }
    }
}

pub struct RouteHarness {
    pub router: Router,
    pub lookup_count: Arc<AtomicUsize>,
    pub event_seen: Arc<Notify>,
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

pub async fn post_response(
    router: &Router,
    token: Option<&str>,
    idempotency_key: Option<&str>,
    body: Value,
) -> Result<TestResponse> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(key) = idempotency_key {
        request = request.header("idempotency-key", key);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string()))?)
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
    let body = serde_json::from_slice(&bytes)?;
    Ok(TestResponse {
        status,
        headers,
        body,
    })
}

pub struct RuntimeStub {
    pub url: String,
    pub turns: Arc<AtomicUsize>,
    release: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
}

impl RuntimeStub {
    pub async fn start(pool: PgPool) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let url = format!("http://{address}");
        let thread_id = Uuid::new_v4().to_string();
        let run_id = Arc::new(Mutex::new(None::<String>));
        let turns = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = oneshot::channel();
        let task_turns = turns.clone();
        let task_run_id = run_id.clone();
        let task_pool = pool.clone();
        let task = tokio::spawn(async move {
            let mut release_rx = Some(release_rx);
            for _ in 0..4 {
                let (stream, _) = listener.accept().await?;
                if handle_runtime_request(
                    stream,
                    &task_pool,
                    &thread_id,
                    &task_run_id,
                    &task_turns,
                    &mut release_rx,
                )
                .await?
                {
                    return Ok(());
                }
            }
            Ok(())
        });
        Ok(Self {
            url,
            turns,
            release: Some(release_tx),
            task: Some(task),
        })
    }

    pub fn release_event_stream(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }

    pub async fn finish(&mut self) -> Result<()> {
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await
            .map_err(|_| "Runtime stub task failed to join")??;
        Ok(())
    }
}

impl Drop for RuntimeStub {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn handle_runtime_request(
    mut stream: TcpStream,
    pool: &PgPool,
    thread_id: &str,
    run_id: &Arc<Mutex<Option<String>>>,
    turns: &Arc<AtomicUsize>,
    release_rx: &mut Option<oneshot::Receiver<()>>,
) -> Result<bool> {
    let (method, path, _headers, body) = read_request(&mut stream).await?;
    let path = path.split('?').next().unwrap_or(&path);
    match (method.as_str(), path) {
        ("POST", "/internal/v1/threads") => {
            let payload: Value = serde_json::from_slice(&body)?;
            let tenant_id = payload["tenantId"]
                .as_str()
                .ok_or("Runtime thread request omitted tenant ID")?;
            let user_id = payload["userId"]
                .as_str()
                .ok_or("Runtime thread request omitted user ID")?;
            let session_id = payload["sessionId"]
                .as_str()
                .ok_or("Runtime thread request omitted session ID")?;
            // Match the Runtime's durable root-identity side effect before it
            // returns the thread ID. The production Gateway reads this row
            // immediately after the HTTP response to bind its capability
            // fingerprint, so a fresh-session route test must preserve it.
            sqlx::query(
                "INSERT INTO assistant_runtime_threads (runtime_thread_id,tenant_id,user_id,session_id,source_kind,import_status) VALUES($1::uuid,$2,$3,$4,'native','pending') ON CONFLICT(runtime_thread_id) DO NOTHING",
            )
            .bind(thread_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(session_id)
            .execute(pool)
            .await?;
            let root_scope: (String, String, String) = sqlx::query_as(
                "SELECT tenant_id,user_id,session_id FROM assistant_runtime_threads WHERE runtime_thread_id=$1::uuid AND deleted_at IS NULL",
            )
            .bind(thread_id)
            .fetch_one(pool)
            .await?;
            if root_scope
                != (
                    tenant_id.to_owned(),
                    user_id.to_owned(),
                    session_id.to_owned(),
                )
            {
                return Err("Runtime thread identity is already owned by another scope".into());
            }
            write_json(&mut stream, "200 OK", &json!({"thread":{"id":thread_id}})).await?;
        }
        ("POST", path) if path.ends_with("/resume") => {
            write_json(&mut stream, "200 OK", &json!({})).await?;
        }
        ("POST", path) if path.ends_with("/turns") => {
            let payload: Value = serde_json::from_slice(&body)?;
            let run = payload["runId"]
                .as_str()
                .ok_or("Runtime turn omitted run ID")?
                .to_owned();
            *run_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(run.clone());
            turns.fetch_add(1, Ordering::SeqCst);
            write_json(&mut stream, "200 OK", &json!({"turn":{"id":run}})).await?;
        }
        ("GET", path) if path.ends_with("/events") => {
            let run = run_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .ok_or("Runtime event request preceded turn start")?;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                .await?;
            let event = json!({
                "event_type": "text_delta",
                "data": {"run_id": run, "content": "accepted partial text"}
            });
            write_chunk(&mut stream, &format!("data: {event}\n\n")).await?;
            let receiver = release_rx
                .take()
                .ok_or("Runtime event stream was opened twice")?;
            let _ = receiver.await;
            stream.write_all(b"0\r\n\r\n").await?;
            stream.shutdown().await?;
            return Ok(true);
        }
        _ => {
            write_json(&mut stream, "404 Not Found", &json!({"error":"not found"})).await?;
        }
    }
    Ok(false)
}

async fn read_request(
    stream: &mut TcpStream,
) -> Result<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).await?;
        head.push(byte[0]);
        if head.len() > 64 * 1024 {
            return Err("Runtime request headers exceeded test bound".into());
        }
    }
    let text = std::str::from_utf8(&head)?;
    let mut lines = text.lines();
    let mut request_line = lines
        .next()
        .ok_or("Runtime request line missing")?
        .split_whitespace();
    let method = request_line
        .next()
        .ok_or("Runtime request method missing")?
        .to_owned();
    let path = request_line
        .next()
        .ok_or("Runtime request path missing")?
        .to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0_usize;
    for line in lines {
        if let Some((name, value)) = line.trim_end_matches('\r').split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse()?;
            }
            headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    if content_length > 1024 * 1024 {
        return Err("Runtime request body exceeded test bound".into());
    }
    let mut body = vec![0; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).await?;
    }
    Ok((method, path, headers, body))
}

async fn write_json(stream: &mut TcpStream, status: &str, value: &Value) -> Result<()> {
    let body = value.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn write_chunk(stream: &mut TcpStream, body: &str) -> Result<()> {
    stream
        .write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes())
        .await?;
    stream.flush().await?;
    Ok(())
}

struct CountedModels {
    inner: Arc<FixtureModels>,
    lookup_count: Arc<AtomicUsize>,
}

impl MetadataModels for CountedModels {
    fn get_model<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> ModelFuture<'a, Option<ModelRow>> {
        Box::pin(async move {
            let result = self.inner.get_model(tenant, model, provider).await;
            self.lookup_count.fetch_add(1, Ordering::SeqCst);
            result
        })
    }

    fn list_models<'a>(
        &'a self,
        tenant: &'a str,
        include_disabled: bool,
    ) -> ModelFuture<'a, Vec<ModelRow>> {
        Box::pin(async move {
            self.inner
                .list_models(tenant, None, include_disabled, None)
                .await
        })
    }
}

struct EventSeenSourceAccess {
    inner: Arc<PgSourceAccess>,
    event_seen: Arc<Notify>,
}

impl SourceAccess for EventSeenSourceAccess {
    fn require_conversation<'a>(
        &'a self,
        scope: &'a ai_platform_gateway::migration_agent::Scope,
        for_execution: bool,
    ) -> AgentFuture<'a, ()> {
        self.inner.require_conversation(scope, for_execution)
    }

    fn require_artifact<'a>(
        &'a self,
        scope: &'a ai_platform_gateway::migration_agent::Scope,
        artifact: &'a Value,
    ) -> AgentFuture<'a, ()> {
        self.inner.require_artifact(scope, artifact)
    }

    fn project_event<'a>(
        &'a self,
        scope: &'a ai_platform_gateway::migration_agent::Scope,
        event: Value,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            let projected = self.inner.project_event(scope, event).await?;
            self.event_seen.notify_one();
            Ok(projected)
        })
    }
}

struct EmptySourceAuthority;

impl SourceAuthority for EmptySourceAuthority {
    fn allowed<'a>(
        &'a self,
        _scope: &'a ai_platform_gateway::migration_agent::Scope,
        refs: &'a SourceRefs,
    ) -> AgentFuture<'a, bool> {
        Box::pin(async move {
            Ok(refs.datasets.is_empty() && refs.documents.is_empty() && refs.versions.is_empty())
        })
    }
}

struct EmptyCatalog;

impl RuntimeCatalog for EmptyCatalog {
    fn fetch<'a>(
        &'a self,
        _scope: &'a ai_platform_gateway::migration_agent::Scope,
        _model: &'a str,
        _revision: i64,
        _allowlist: Option<&'a [Value]>,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async { Ok(json!({"tools": [], "mcp": [], "deferred": []})) })
    }
}

struct NoopPricingCache;

impl ModelPricingCache for NoopPricingCache {
    fn invalidate_cache(&self) -> std::result::Result<(), ModelServiceError> {
        Ok(())
    }
}

struct NoGoogleAuth;

impl ProviderGoogleAuth for NoGoogleAuth {
    fn acquire<'a>(
        &'a self,
        _service_account: Option<Value>,
        _scopes: &'a [&'a str],
    ) -> ServiceFuture<'a, GoogleCredentialState> {
        Box::pin(async { Err(ServiceError::ProviderNotFound) })
    }

    fn refresh<'a>(&'a self, _credentials: GoogleCredentialState) -> ServiceFuture<'a, String> {
        Box::pin(async { Err(ServiceError::ProviderNotFound) })
    }
}

struct EmptyProviderEnvironment;

impl ProviderEnvironment for EmptyProviderEnvironment {
    fn get(&self, _name: &str) -> Option<String> {
        None
    }
}

struct UtcProviderClock;

impl ProviderClock for UtcProviderClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

fn loopback_url(name: &str, schemes: &[&str]) -> Result<String> {
    let value = std::env::var(name)
        .map_err(|_| format!("{name} must explicitly select loopback test infrastructure"))?;
    let parsed = reqwest::Url::parse(&value).map_err(|_| format!("invalid {name}"))?;
    if !schemes.contains(&parsed.scheme())
        || !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
    {
        return Err(format!("{name} must use a supported loopback URL").into());
    }
    Ok(value)
}

fn loopback_dsn(name: &str) -> Result<String> {
    let value = std::env::var(name)
        .map_err(|_| format!("{name} must explicitly select a scoped loopback database DSN"))?;
    let parsed = reqwest::Url::parse(&value).map_err(|_| format!("invalid {name}"))?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql")
        || parsed.host_str() != Some("127.0.0.1")
    {
        return Err(
            format!("{name} must use the project's numeric loopback PostgreSQL address").into(),
        );
    }
    Ok(value)
}

fn dsn_url(dsn: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(dsn).map_err(|_| "invalid PostgreSQL test DSN")?;
    if !matches!(url.scheme(), "postgres" | "postgresql") || url.host_str() != Some("127.0.0.1") {
        return Err("PostgreSQL test DSNs must use the project's numeric loopback address".into());
    }
    Ok(url)
}

fn database_name(dsn: &str) -> Result<String> {
    let url = dsn_url(dsn)?;
    let name = url
        .path()
        .strip_prefix('/')
        .filter(|name| !name.is_empty())
        .ok_or("PostgreSQL test DSN must name a database")?;
    if !safe_database_identifier(name) {
        return Err("PostgreSQL test DSN has an unsupported database identifier".into());
    }
    Ok(name.to_owned())
}

fn safe_database_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn database_url_for(dsn: &str, database: &str) -> Result<String> {
    if !safe_database_identifier(database) {
        return Err("unsupported isolated database identifier".into());
    }
    let mut url = dsn_url(dsn)?;
    url.set_path(&format!("/{database}"));
    Ok(url.to_string())
}

fn validate_source_dsns(
    template_dsn: &str,
    gateway_dsn: &str,
    admin_dsn: &str,
    runtime_dsn: &str,
    source_database: &str,
    role_prefix: &str,
) -> Result<()> {
    let template = dsn_url(template_dsn)?;
    let gateway = dsn_url(gateway_dsn)?;
    let admin = dsn_url(admin_dsn)?;
    let runtime = dsn_url(runtime_dsn)?;
    let expected_migrator = format!("{role_prefix}migrator");
    let expected_gateway = format!("{role_prefix}gateway");
    let expected_runtime = format!("{role_prefix}runtime");
    let same_server = |other: &reqwest::Url| {
        other.host_str() == template.host_str()
            && other.port_or_known_default() == template.port_or_known_default()
    };
    if database_name(template_dsn)? != source_database
        || database_name(gateway_dsn)? != source_database
        || database_name(admin_dsn)? != source_database
        || database_name(runtime_dsn)? != source_database
        || !same_server(&gateway)
        || !same_server(&admin)
        || !same_server(&runtime)
        || template.username() != expected_migrator.as_str()
        || gateway.username() != expected_gateway.as_str()
        || runtime.username() != expected_runtime.as_str()
        || admin.username().is_empty()
    {
        return Err("explicit template and scoped role DSNs must identify this project's same source database".into());
    }
    Ok(())
}

fn repository_authority_paths() -> Result<AuthorityPaths> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = manifest_dir
        .ancestors()
        .nth(4)
        .ok_or("cannot resolve repository root")?
        .canonicalize()?;
    Ok(AuthorityPaths {
        database_dir: repository_root.join("database"),
    })
}

async fn connect_pool(dsn: &str, max_connections: u32) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(dsn)
        .await
        .map_err(|_| {
            "scoped PostgreSQL connection to the isolated Responses database failed".into()
        })
}

async fn provision_cloned_database(
    dsn: &str,
    paths: &AuthorityPaths,
    role_prefix: &str,
) -> Result<()> {
    let mut connection = sqlx::PgConnection::connect(dsn)
        .await
        .map_err(|_| "isolated database bootstrap connection failed; details withheld")?;
    let mut transaction = connection.begin().await?;
    // Exclude only membership row OIDs, which a native REVOKE/GRANT replaces.
    // All permission-bearing columns and password verifiers remain compared.
    const CATALOG: &str = "SELECT jsonb_build_object('roles', (SELECT jsonb_agg(to_jsonb(r) ORDER BY r.rolname) FROM pg_catalog.pg_authid r), 'memberships', (SELECT jsonb_agg(to_jsonb(m)-'oid' ORDER BY m.roleid,m.member,m.grantor) FROM pg_catalog.pg_auth_members m))";
    let before: Value = sqlx::query_scalar(CATALOG)
        .fetch_one(&mut *transaction)
        .await?;
    bootstrap::provision_roles_admin(&mut transaction, paths, role_prefix).await?;
    let after: Value = sqlx::query_scalar(CATALOG)
        .fetch_one(&mut *transaction)
        .await?;
    if before != after {
        transaction.rollback().await?;
        return Err("isolated bootstrap would change global role authority; rolled back".into());
    }
    transaction.commit().await?;
    connection.close().await?;
    Ok(())
}

async fn verify_epoch_24(
    dsn: &str,
    paths: &AuthorityPaths,
    role_prefix: &str,
    label: &str,
) -> Result<()> {
    let authority = MigrationAuthority::new(dsn.to_owned(), paths.clone(), role_prefix.to_owned())?;
    commands::verify(&authority, DEFAULT_BASELINE_ID).await?;
    let mut connection = authority.connect(true).await?;
    let applied = authority
        .applied_changes(&mut connection, DEFAULT_BASELINE_ID)
        .await?;
    connection.close().await?;
    if applied.len() != 24 || applied.keys().copied().ne(1..=24) {
        return Err(format!("{label} is not the exact native epoch-24 database").into());
    }
    Ok(())
}

async fn verify_gateway_admission(
    dsn: &str,
    paths: &AuthorityPaths,
    role_prefix: &str,
) -> Result<()> {
    let authority = MigrationAuthority::new(dsn.to_owned(), paths.clone(), role_prefix.to_owned())?;
    commands::startup_check(&authority, "gateway").await?;
    Ok(())
}

fn actor_fixture(tenant_id: &str, user_id: &str) -> ActorFixture {
    ActorFixture {
        actor: Actor {
            tenant_id: tenant_id.to_owned(),
            user_id: user_id.to_owned(),
            roles: vec!["user".into()],
            tier: "normal".into(),
        },
        token: String::new(),
        jti: String::new(),
    }
}

async fn seed_accounts_and_model(
    admin: &PgPool,
    tenant_a: &str,
    user_a: &str,
    tenant_b: &str,
    user_b: &str,
    model_id: &str,
    provider_id: &str,
) -> Result<()> {
    for (tenant, user) in [(tenant_a, user_a), (tenant_b, user_b)] {
        sqlx::query("INSERT INTO gateway.tenants(tenant_id,name,status) VALUES($1,$2,'active')")
            .bind(tenant)
            .bind(format!("Responses route fixture {tenant}"))
            .execute(admin)
            .await?;
        sqlx::query(
            "INSERT INTO gateway.users(user_id,username,email,display_name,tenant_id,status,email_verified,created_by) VALUES($1,$1,$2,$3,$4,'active',true,'responses-route-fixture')",
        )
        .bind(user)
        .bind(format!("{user}@responses.invalid"))
        .bind("Responses route test user")
        .bind(tenant)
        .execute(admin)
        .await?;
        sqlx::query(
            "INSERT INTO gateway.user_roles(user_id,role_name,granted_by) VALUES($1,'user','responses-route-fixture')",
        )
        .bind(user)
        .execute(admin)
        .await?;
    }
    sqlx::query(
        "INSERT INTO gateway.llm_providers(tenant_id,provider_id,display_name,api_type,base_url,metadata,is_enabled) VALUES($1,$2,'Responses route fixture','openai','http://127.0.0.1:1/v1','{}',true)",
    )
    .bind(tenant_a)
    .bind(provider_id)
    .execute(admin)
    .await?;
    sqlx::query(
        "INSERT INTO gateway.llm_models(tenant_id,provider_id,model_id,display_name,access_level,is_enabled,supports_tools,supports_vision,catalog_capabilities,capability_overrides,capability_revision) VALUES($1,$2,$3,'Responses route fixture','public',true,true,false,'{}','{}',1)",
    )
    .bind(tenant_a)
    .bind(provider_id)
    .bind(model_id)
    .execute(admin)
    .await?;
    Ok(())
}

async fn issue_token(
    codec: &TokenCodec,
    redis: &ConnectionManager,
    actor: &Actor,
) -> Result<String> {
    let now = unix_now()?;
    let token = codec.issue(
        Claims {
            sub: actor.user_id.clone(),
            user_id: actor.user_id.clone(),
            tenant_id: actor.tenant_id.clone(),
            email: None,
            roles: actor.roles.clone(),
            permissions: Vec::new(),
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
    let claims = codec.verify(&token, now)?;
    RedisSessions::new(redis.clone())
        .save(
            &claims.jti,
            &actor.user_id,
            &json!({"user_id": actor.user_id, "credential_revision": null}),
            300,
        )
        .await?;
    Ok(token)
}

fn unix_now() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

fn idempotency_scope(tenant: &str, user: &str, key: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{tenant}\n{user}\n/v1/responses\n{key}").as_bytes())
    )
}

async fn drop_database(admin: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(&format!(
        "DROP DATABASE IF EXISTS \"{database_name}\" WITH (FORCE)"
    ))
    .execute(admin)
    .await?;
    Ok(())
}
