//! Opt-in native PostgreSQL/Redis authority regressions. No infrastructure is started.
//! Requires AUTH_PARITY_DISPOSABLE=1, AUTH_PARITY_ADMIN_DATABASE_URL and AUTH_PARITY_REDIS_URL,
//! both loopback. Creates/drops only a random, test-owned database; never alters an existing schema.
use ai_platform_auth::{
    PasswordEngine, TokenCodec, accounts::PgAccounts, service::AuthService, sessions::RedisSessions,
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::BTreeMap, str::FromStr, sync::Arc};
use tower::ServiceExt;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn loopback(name: &str, scheme: &[&str]) -> Result<String> {
    let raw = std::env::var(name)
        .map_err(|_| format!("{name} must select explicit disposable loopback infrastructure"))?;
    let parsed = reqwest::Url::parse(&raw).map_err(|_| "invalid test infrastructure URL")?;
    if !scheme.contains(&parsed.scheme())
        || !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
    {
        return Err("test infrastructure must be loopback".into());
    }
    Ok(raw)
}
async fn isolated<F, Fut>(run: F) -> Result<()>
where
    F: FnOnce(PgPool, redis::aio::ConnectionManager) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if std::env::var("AUTH_PARITY_DISPOSABLE").as_deref() != Ok("1") {
        return Err("AUTH_PARITY_DISPOSABLE=1 is required".into());
    }
    let url = loopback(
        "AUTH_PARITY_ADMIN_DATABASE_URL",
        &["postgres", "postgresql"],
    )?;
    let redis_url = loopback("AUTH_PARITY_REDIS_URL", &["redis"])?;
    let options =
        PgConnectOptions::from_str(&url).map_err(|_| "invalid test database configuration")?;
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .map_err(|_| "test administrator database unavailable")?;
    let name = format!("gateway_auth_parity_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .map_err(|_| "cannot create isolated test database")?;
    let outcome=std::panic::AssertUnwindSafe(async {
  let pool=PgPoolOptions::new().max_connections(3).connect_with(options.database(&name)).await.map_err(|_|"isolated test database unavailable")?;
  let result=async {
   sqlx::raw_sql("CREATE SCHEMA gateway; CREATE TABLE gateway.users(user_id text PRIMARY KEY,tenant_id text NOT NULL,email text,display_name text,department text,tier text NOT NULL DEFAULT 'normal',roles text[] NOT NULL DEFAULT '{}',permissions text[] NOT NULL DEFAULT '{}',status text NOT NULL DEFAULT 'active',password_hash text,force_password_change bool NOT NULL DEFAULT false,login_attempts integer NOT NULL DEFAULT 0,locked_until timestamptz,password_changed_at timestamptz); CREATE TABLE gateway.login_audit(id bigserial PRIMARY KEY,user_id text,email text,action text,ip_address text,user_agent text,details jsonb);").execute(&pool).await?;
   let redis=redis::Client::open(redis_url).map_err(|_|"invalid test Redis configuration")?.get_connection_manager().await.map_err(|_|"isolated test Redis unavailable")?;
   run(pool.clone(),redis).await
  }.await;pool.close().await;result
 }).catch_unwind().await;
    let cleanup = sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&admin)
        .await;
    admin.close().await;
    match outcome {
        Ok(result) => {
            cleanup.map_err(|_| "isolated database cleanup failed")?;
            result
        }
        Err(panic) => {
            let _ = cleanup;
            std::panic::resume_unwind(panic)
        }
    }
}
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live acceptance claimed"]
async fn login_does_not_jit_create_missing_user_with_default_password() -> Result<()> {
    isolated(|pool,redis|async move{
 let service=Arc::new(AuthService::new(PgAccounts::new(pool.clone()),RedisSessions::new(redis),TokenCodec::new(b"auth-parity-synthetic-signing-key-only",None,None)?,PasswordEngine::new(1)?,"example.com".into()));
 let response=ai_platform_auth::http::router(service,BTreeMap::new()).oneshot(Request::builder().method("POST").uri("/api/v1/auth/login").header("content-type","application/json").body(Body::from(json!({"email":"new.user@example.com","password":"bootstrap-probe-password"}).to_string()))?).await?;
 assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
 let body:Value=serde_json::from_slice(&to_bytes(response.into_body(),4096).await?)?;assert_eq!(body["detail"],"Invalid email or password");
 let count:i64=sqlx::query_scalar("SELECT count(*) FROM gateway.users").fetch_one(&pool).await?;assert_eq!(count,0);
 let details:Value=sqlx::query_scalar("SELECT details FROM gateway.login_audit ORDER BY id DESC LIMIT 1").fetch_one(&pool).await?;assert_eq!(details["reason"],"user_not_found");Ok(())
}).await
}

/// Protected JWT/API-key authority must not restore stale users.roles or
/// users.permissions after all matching user_roles assignments have expired.
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live acceptance claimed"]
async fn protected_authority_uses_current_assignments_for_jwt_and_bound_keys() -> Result<()> {
    isolated(|pool, redis| async move {
        use ai_platform_auth::{Claims, contract::VALIDATE};
        use sha2::{Digest, Sha256};

        sqlx::raw_sql(
            "CREATE TABLE gateway.tenants(tenant_id text PRIMARY KEY,status text NOT NULL);
             CREATE TABLE gateway.user_roles(user_id text,role_name text,expires_at timestamptz,granted_at timestamptz NOT NULL DEFAULT NOW());
             CREATE TABLE gateway.role_permissions(role_name text,permission_code text);
             CREATE TABLE gateway.rbac_roles(role_name text,permissions text[]);
             CREATE TABLE gateway.user_permissions(user_id text,permission_code text,expires_at timestamptz);
             CREATE TABLE gateway.api_keys(id serial PRIMARY KEY,key_id text,user_id text,tenant_id text,roles text[] NOT NULL,permissions text[] NOT NULL,tier text NOT NULL,rate_limit jsonb,allowed_services text[] NOT NULL DEFAULT '{}',allowed_models text[] NOT NULL DEFAULT '{}',scopes jsonb,key_hash text,enabled boolean,expires_at timestamptz);
             INSERT INTO gateway.tenants VALUES('tenant-a','active');
             INSERT INTO gateway.users(user_id,tenant_id,roles,permissions,tier) VALUES('user-a','tenant-a',ARRAY['admin'],ARRAY['console:users:edit'],'normal');
             INSERT INTO gateway.user_roles(user_id,role_name,expires_at) VALUES('user-a','admin',NOW()-INTERVAL '1 minute');
             INSERT INTO gateway.role_permissions(role_name,permission_code) VALUES('admin','console:users:edit');
             INSERT INTO gateway.rbac_roles(role_name,permissions) VALUES('admin',ARRAY['console:users:edit']);
             INSERT INTO gateway.user_permissions(user_id,permission_code) VALUES('user-a','console:profile:view');",
        )
        .execute(&pool)
        .await?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let secret = b"auth-current-authority-test-secret-only";
        let codec = TokenCodec::new(secret, None, None)?;
        let token = codec.issue(
            Claims {
                sub: "user-a".into(),
                user_id: "user-a".into(),
                tenant_id: "tenant-a".into(),
                email: None,
                roles: vec!["admin".into()],
                permissions: vec!["console:users:edit".into()],
                tier: "admin".into(),
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
        let sessions = RedisSessions::new(redis.clone());
        sessions
            .save(
                &claims.jti,
                "user-a",
                &json!({"user_id":"user-a","credential_revision":null}),
                10_800,
            )
            .await?;
        let auth = Arc::new(AuthService::new(
            PgAccounts::new(pool.clone()),
            sessions,
            codec,
            PasswordEngine::new(1)?,
            "example.com".into(),
        ));
        let response = ai_platform_auth::http::router(auth, BTreeMap::new())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(VALIDATE)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        assert_eq!(body["roles"], json!(["console:profile:view"]));
        assert_eq!(body["tier"], "normal");

        let raw_key = "current-authority-key";
        sqlx::query("INSERT INTO gateway.api_keys(key_id,user_id,tenant_id,roles,permissions,tier,key_hash,enabled) VALUES('key-a','user-a','tenant-a',ARRAY['key-user'],ARRAY['knowledge:read'],'normal',$1,true)")
            .bind(format!("{:x}", Sha256::digest(raw_key.as_bytes())))
            .execute(&pool)
            .await?;
        let key = PgAccounts::new(pool)
            .bound_api_key(raw_key)
            .await?;
        assert!(key.roles.iter().any(|role| role == "key-user"));
        assert!(key.roles.iter().any(|role| role == "knowledge:read"));
        assert!(key.roles.iter().any(|role| role == "console:profile:view"));
        assert!(!key.roles.iter().any(|role| role == "admin"));
        assert!(!key
            .effective_permissions
            .iter()
            .any(|permission| permission == "console:users:edit"));
        Ok(())
    })
    .await
}

fn cache_request(
    path: &str,
) -> ai_platform_gateway::migration_compat::transparent_proxy::ProxyRequest {
    use ai_platform_gateway::migration_compat::{
        common::CompatUser,
        transparent_proxy::{ProxyRequest, ProxyServiceConfig},
    };
    ProxyRequest {
        config: ProxyServiceConfig {
            service_id: "agent".into(),
            service_name: "Agent".into(),
            upstream_url: "http://localhost:2024".into(),
            cache_enabled: true,
            cache_ttl: 120,
            ..Default::default()
        },
        user: CompatUser {
            user_id: "user_1".into(),
            tenant_id: "tenant_1".into(),
            ..Default::default()
        },
        api_key_id: None,
        method: axum::http::Method::POST,
        path: path.into(),
        body: axum::body::Bytes::from_static(
            br#"{"input":{"messages":[{"role":"user","content":"hello"}]},"assistant_id":"flash"}"#,
        ),
        headers: axum::http::HeaderMap::new(),
        query: vec![],
        stream: false,
        langgraph_body_prepared: false,
        effective_model: None,
        effective_provider: None,
        capacity_budgets: vec![],
    }
}
#[tokio::test]
async fn response_cache_bypasses_unsupported_path_without_database_access() {
    use ai_platform_gateway::migration_compat::response_cache::ResponseCache;
    let pool = PgPoolOptions::new()
        .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    let cache = ResponseCache {
        pool,
        enabled: true,
    };
    let mut request = cache_request("/assistants/search");
    request.body = axum::body::Bytes::from_static(b"{}");
    let (status, hash, cached) = cache.get(&request, &json!({}), false).await;
    assert_eq!(status, "BYPASS");
    assert!(hash.is_none());
    assert!(cached.is_none());
}
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live acceptance claimed"]
async fn response_cache_miss_then_hit_preserves_exact_response_bytes() -> Result<()> {
    isolated(|pool,_|async move{
 use ai_platform_gateway::migration_compat::response_cache::ResponseCache;
 sqlx::raw_sql("CREATE TABLE semantic_cache(service_id text,input_hash text,input_text text,output_text text,output_data jsonb,metadata jsonb,expires_at timestamptz,hit_count bigint NOT NULL DEFAULT 0,last_hit_at timestamptz,PRIMARY KEY(service_id,input_hash));").execute(&pool).await?;
 let cache=ResponseCache{pool,enabled:true};let request=cache_request("/runs/wait");let parsed:Value=serde_json::from_slice(&request.body)?;
 let(status,hash,cached)=cache.get(&request,&parsed,false).await;assert_eq!(status,"MISS");assert!(hash.as_ref().is_some_and(|h|!h.is_empty()));assert!(cached.is_none());
 let mut headers=axum::http::HeaderMap::new();headers.insert("content-type",axum::http::HeaderValue::from_static("application/json"));let body=br#"{"messages":[{"type":"ai","content":"ok"}]}"#;
 cache.save(&request,&parsed,hash.as_deref(),StatusCode::OK,&headers,body,false).await;
 let(status,_,cached)=cache.get(&request,&parsed,false).await;assert_eq!(status,"HIT");assert_eq!(cached.expect("cache hit response").body.as_ref(),body);Ok(())
}).await
}

async fn introspection_schema(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql("CREATE TABLE gateway.tenants(tenant_id text PRIMARY KEY,status text NOT NULL); CREATE TABLE gateway.user_roles(user_id text,role_name text,expires_at timestamptz,granted_at timestamptz NOT NULL DEFAULT NOW()); CREATE TABLE gateway.role_permissions(role_name text,permission_code text); CREATE TABLE gateway.rbac_roles(role_name text,permissions text[]); CREATE TABLE gateway.user_permissions(user_id text,permission_code text,expires_at timestamptz); CREATE TABLE gateway.api_keys(id serial PRIMARY KEY,user_id text,tenant_id text,roles text[],permissions text[],tier text,key_hash text,enabled bool,expires_at timestamptz,last_used_at timestamptz,use_count bigint); INSERT INTO gateway.tenants VALUES('tenant-a','active');").execute(pool).await?;
    Ok(())
}
fn introspection_router(
    pool: PgPool,
    redis: redis::aio::ConnectionManager,
) -> Result<axum::Router> {
    introspection_router_with_sessions(pool, redis, None)
}
fn introspection_router_with_sessions(
    pool: PgPool,
    redis: redis::aio::ConnectionManager,
    sessions: Option<RedisSessions>,
) -> Result<axum::Router> {
    use ai_platform_gateway::migration_host::{
        credential_introspection::CredentialIntrospection, jwt::JwtDecoder,
    };
    let secret = "phase-one-auth-test-secret-not-a-real-credential";
    let resolver = Arc::new(CredentialIntrospection {
        pool: pool.clone(),
        decoder: Arc::new(
            JwtDecoder::new(secret, &["HS256".into()], None, None)
                .map_err(std::io::Error::other)?,
        ),
        sessions,
        jwt_enabled: true,
        api_key_enabled: true,
        api_key_header: axum::http::HeaderName::from_static("x-api-key"),
        static_key_hashes: vec![],
        static_key_role: "user".into(),
    });
    let service = Arc::new(AuthService::new(
        PgAccounts::new(pool),
        RedisSessions::new(redis),
        TokenCodec::new(secret.as_bytes(), None, None)?,
        PasswordEngine::new(1)?,
        "example.com".into(),
    ));
    Ok(ai_platform_auth::http::router_with_introspection(
        service,
        BTreeMap::new(),
        resolver,
    ))
}
fn introspection_request(mode: &str) -> Result<Request<Body>> {
    let builder = Request::builder()
        .uri("/api/v1/auth/me")
        .header("x-tenant-id", "forged-tenant")
        .header("x-user-id", "forged-user");
    let builder = if mode == "jwt" {
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &json!({"sub":"eval-user","tenant_id":"tenant-a","roles":["user"],"exp":chrono::Utc::now().timestamp()+300}),
            &jsonwebtoken::EncodingKey::from_secret(
                b"phase-one-auth-test-secret-not-a-real-credential",
            ),
        )?;
        builder.header("authorization", format!("Bearer {token}"))
    } else {
        builder.header("x-api-key", "phase-one-test-api-key")
    };
    Ok(builder.body(Body::empty())?)
}
/// Original test_auth_me_returns_tenant_from_verified_credentials: all four profile/mode rows.
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live acceptance claimed"]
async fn auth_me_returns_tenant_from_verified_credentials_with_and_without_profile() -> Result<()> {
    isolated(|pool,redis|async move {
        use sha2::{Digest,Sha256};
        introspection_schema(&pool).await?;
        sqlx::query("INSERT INTO gateway.api_keys(user_id,tenant_id,roles,permissions,tier,key_hash,enabled) VALUES('eval-user','tenant-a',ARRAY['user'],'{}','normal',$1,true)").bind(format!("{:x}",Sha256::digest(b"phase-one-test-api-key"))).execute(&pool).await?;
        let router = introspection_router(pool.clone(),redis)?;
        for profile in [true,false] {
            sqlx::query("DELETE FROM gateway.users").execute(&pool).await?;
            if profile { sqlx::query("INSERT INTO gateway.users(user_id,tenant_id,roles) VALUES('eval-user','tenant-a',ARRAY['user'])").execute(&pool).await?; }
            for mode in ["jwt","api_key"] {
                let response = router.clone().oneshot(introspection_request(mode)?).await?;
                assert_eq!(response.status(),StatusCode::OK,"mode={mode}, profile={profile}");
                let body:Value = serde_json::from_slice(&to_bytes(response.into_body(),4096).await?)?;
                assert_eq!(body["tenant_id"],"tenant-a"); assert_eq!(body["user_id"],"eval-user");
            }
        }
        Ok(())
    }).await
}
/// Read-only introspection must still fail closed for a moved or disabled existing profile.
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live acceptance claimed"]
async fn auth_me_rejects_existing_inactive_or_moved_account() -> Result<()> {
    isolated(|pool,redis|async move {
        use sha2::{Digest,Sha256};
        introspection_schema(&pool).await?;
        sqlx::query("INSERT INTO gateway.api_keys(user_id,tenant_id,roles,permissions,tier,key_hash,enabled) VALUES('eval-user','tenant-a',ARRAY['user'],'{}','normal',$1,true)").bind(format!("{:x}",Sha256::digest(b"phase-one-test-api-key"))).execute(&pool).await?;
        let router = introspection_router(pool.clone(),redis)?;
        for (tenant,status) in [("tenant-a","inactive"),("tenant-b","active")] {
            sqlx::query("DELETE FROM gateway.users").execute(&pool).await?;
            sqlx::query("INSERT INTO gateway.users(user_id,tenant_id,status,roles) VALUES('eval-user',$1,$2,ARRAY['user'])").bind(tenant).bind(status).execute(&pool).await?;
            for mode in ["jwt","api_key"] { assert_eq!(router.clone().oneshot(introspection_request(mode)?).await?.status(),StatusCode::UNAUTHORIZED); }
        }
        sqlx::query("ALTER TABLE gateway.users RENAME TO unavailable_users").execute(&pool).await?;
        for mode in ["jwt", "api_key"] {
            let response = router.clone().oneshot(introspection_request(mode)?).await?;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body = to_bytes(response.into_body(), 4096).await?;
            let text = String::from_utf8_lossy(&body);
            assert!(!text.contains("unavailable_users"));
            assert!(!text.contains("gateway.users"));
        }
        Ok(())
    }).await
}

/// `/auth/me` must honor the same native session revision as protected JWT
/// admission while retaining external identities without a local account.
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live acceptance claimed"]
async fn auth_me_revokes_local_session_after_reset_disable_reenable_and_delete() -> Result<()> {
    isolated(|pool, redis| async move {
        use ai_platform_auth::{Claims, contract::VALIDATE};
        use chrono::{DateTime, Utc};

        introspection_schema(&pool).await?;
        sqlx::query("INSERT INTO gateway.users(user_id,tenant_id,roles,permissions,status,password_changed_at) VALUES('local-user','tenant-a',ARRAY['admin'],ARRAY['console:users:view'],'active',clock_timestamp())")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO gateway.user_roles(user_id,role_name,expires_at) VALUES('local-user','admin',NOW()-INTERVAL '1 minute')")
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO gateway.role_permissions(role_name,permission_code) VALUES('admin','console:users:view')")
            .execute(&pool)
            .await?;
        let revision: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT password_changed_at FROM gateway.users WHERE user_id='local-user'",
        )
        .fetch_one(&pool)
        .await?;

        let redis_sessions = RedisSessions::new(redis.clone());
        let router = introspection_router_with_sessions(
            pool.clone(),
            redis.clone(),
            Some(redis_sessions.clone()),
        )?;
        let codec = TokenCodec::new(
            b"phase-one-auth-test-secret-not-a-real-credential",
            None,
            None,
        )?;
        let now = chrono::Utc::now().timestamp() as u64;
        let token = codec.issue(
            Claims {
                sub: "local-user".into(),
                user_id: "local-user".into(),
                tenant_id: "tenant-a".into(),
                email: None,
                roles: vec!["admin".into()],
                permissions: vec!["console:users:view".into()],
                tier: "admin".into(),
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
        let session_record = json!({
            "user_id": "local-user",
            "credential_revision": revision.map(|value| value.to_rfc3339()),
        });
        redis_sessions
            .save(&claims.jti, "local-user", &session_record, 10_800)
            .await?;

        let me = |token: &str| {
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/me")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
        };
        let validate = |token: &str| {
            Request::builder()
                .method("POST")
                .uri(VALIDATE)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
        };

        let current = router.clone().oneshot(me(&token)?).await?;
        assert_eq!(current.status(), StatusCode::OK);
        let current_body: Value =
            serde_json::from_slice(&to_bytes(current.into_body(), 4096).await?)?;
        // Expired role assignments must not be resurrected from users.roles or
        // the JWT's earlier role snapshot on this local identity path.
        assert_eq!(current_body["roles"], json!([]));
        assert_eq!(current_body["permissions"], json!([]));
        assert_eq!(router.clone().oneshot(validate(&token)?).await?.status(), StatusCode::OK);

        // Password reset advances password_changed_at. The same old session
        // must remain invalid after disable->enable toggles the account status.
        sqlx::query("UPDATE gateway.users SET password_changed_at=clock_timestamp() WHERE user_id='local-user'")
            .execute(&pool)
            .await?;
        assert_eq!(router.clone().oneshot(me(&token)?).await?.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(router.clone().oneshot(validate(&token)?).await?.status(), StatusCode::UNAUTHORIZED);
        sqlx::query("UPDATE gateway.users SET status='inactive',password_changed_at=clock_timestamp() WHERE user_id='local-user'")
            .execute(&pool)
            .await?;
        assert_eq!(router.clone().oneshot(me(&token)?).await?.status(), StatusCode::UNAUTHORIZED);
        sqlx::query("UPDATE gateway.users SET status='active' WHERE user_id='local-user'")
            .execute(&pool)
            .await?;
        assert_eq!(router.clone().oneshot(me(&token)?).await?.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(router.clone().oneshot(validate(&token)?).await?.status(), StatusCode::UNAUTHORIZED);
        sqlx::query("DELETE FROM gateway.users WHERE user_id='local-user'")
            .execute(&pool)
            .await?;
        assert_eq!(router.clone().oneshot(me(&token)?).await?.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(router.clone().oneshot(validate(&token)?).await?.status(), StatusCode::UNAUTHORIZED);

        // Legacy external identities without a native local session marker keep
        // their existing no-profile introspection contract.
        let external = router
            .oneshot(introspection_request("jwt")?)
            .await?;
        assert_eq!(external.status(), StatusCode::OK);
        let external_body: Value =
            serde_json::from_slice(&to_bytes(external.into_body(), 4096).await?)?;
        assert_eq!(external_body["user_id"], "eval-user");

        redis_sessions.revoke(&claims.jti, "local-user").await?;
        Ok(())
    })
    .await
}

/// Source image worker claim assertions exercised against real transactional SQL.
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; no live acceptance claimed"]
async fn image_claim_retries_only_pre_dispatch_work_and_fences_dispatch() -> Result<()> {
    isolated(|pool,_|async move {
        use ai_platform_gateway::migration_content::{ContentDatabase,images_state::ImageRepository};
        sqlx::raw_sql("CREATE SCHEMA assistant; CREATE TABLE assistant.image_tasks(task_id text PRIMARY KEY,tenant_id text,user_id text,owner_scope text,runtime_scope_version integer,status text,locked_until timestamptz,attempt_count integer,max_attempts integer,created_at timestamptz DEFAULT NOW(),updated_at timestamptz,started_at timestamptz,completed_at timestamptz,error text,error_code text);
        INSERT INTO assistant.image_tasks(task_id,tenant_id,user_id,owner_scope,runtime_scope_version,status,locked_until,attempt_count,max_attempts) VALUES
        ('imt_retry','row-tenant','row-user','scope',1,'claimed',NOW()-INTERVAL '1 minute',1,3),
        ('imt_dispatched','row-tenant','row-user','scope',1,'running',NOW()-INTERVAL '1 minute',1,3),
        ('imt_exhausted','row-tenant','row-user','scope',1,'claimed',NOW()-INTERVAL '1 minute',3,3);").execute(&pool).await?;
        let repository=ImageRepository{database:ContentDatabase::new(pool.clone(),pool.clone())?};
        let rows=repository.claim_pending(1,30).await?;assert_eq!(rows.len(),1);assert_eq!(rows[0]["task_id"],"imt_retry");assert_eq!(rows[0]["status"],"claimed");assert_eq!(rows[0]["attempt_count"],2);
        let lost:Value=sqlx::query_scalar("SELECT to_jsonb(t) FROM assistant.image_tasks t WHERE task_id='imt_dispatched'").fetch_one(&pool).await?;
        assert_eq!(lost["status"],"unknown");assert_eq!(lost["error"],"image worker lost after provider dispatch");assert_eq!(lost["attempt_count"],1);
        let exhausted:String=sqlx::query_scalar("SELECT status FROM assistant.image_tasks WHERE task_id='imt_exhausted'").fetch_one(&pool).await?;assert_eq!(exhausted,"dead_letter");
        let mut forged=rows[0].clone();forged["tenant_id"]=json!("wrong-tenant");assert!(!repository.dispatch_claim(&forged).await?);
        assert!(repository.dispatch_claim(&rows[0]).await?);assert!(!repository.dispatch_claim(&rows[0]).await?);
        let status:String=sqlx::query_scalar("SELECT status FROM assistant.image_tasks WHERE task_id='imt_retry'").fetch_one(&pool).await?;assert_eq!(status,"running");
        Ok(())
    }).await
}

async fn accounting_fixture(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql("CREATE TABLE billing_events(event_id varchar(255) PRIMARY KEY,request_id text NOT NULL,service_id text NOT NULL,user_id text,tenant_id text,input_tokens integer NOT NULL,output_tokens integer NOT NULL,total_tokens integer NOT NULL,model text,assistant_id text,status text,cost_amount numeric,cost_currency text,metadata jsonb,event_time timestamptz); CREATE TABLE user_quotas(tenant_id text,user_id text,current_daily_tokens bigint DEFAULT 0,current_monthly_tokens bigint DEFAULT 0,current_monthly_cost_cents bigint DEFAULT 0,current_daily_requests bigint DEFAULT 0,updated_at timestamptz); INSERT INTO user_quotas(tenant_id,user_id) VALUES('tenant-a','user-a');").execute(pool).await?;
    Ok(())
}
fn accounting_usage() -> ai_platform_gateway_foundation::usage_recorder::UsageRecord {
    let mut r = ai_platform_gateway_foundation::usage_recorder::UsageRecord::new(
        "tenant-a",
        "user-a",
        "model-a",
        chrono::Utc::now(),
    );
    r.request_id = "stable-request".into();
    r.service_id = "service-a".into();
    r.input_tokens = 7;
    r.output_tokens = 3;
    r
}
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; atomic receipt acceptance unrun"]
async fn accounting_receipt_and_mutation_commit_once_and_authoritative_replay_skips_duplicate()
-> Result<()> {
    use ai_platform_gateway_foundation::usage_recorder::{
        AccountingStage, PgUsagePersistence, StageStatus, UsagePersistence, UsageRecorderError,
        stage_receipts::failure_id,
    };
    isolated(|pool,_| async move {
        accounting_fixture(&pool).await?;
        let writer=PgUsagePersistence::new(pool.clone());let r=accounting_usage();let stage=AccountingStage::QuotaCounters;
        writer.post_write(stage,&[r.clone()]).await?;
        writer.post_write(stage,&[r.clone()]).await?;
        let totals:(i64,i64)=sqlx::query_as("SELECT current_daily_tokens,current_daily_requests FROM user_quotas").fetch_one(&pool).await?;assert_eq!(totals,(10,1));
        // Simulate a caller losing a stage acknowledgement after the atomic mutation.
        // The stored applied receipt, not a process-local flag, suppresses charging again.
        writer.reconcile(stage,&[r.clone()],&UsageRecorderError::Collaborator("sanitized fixture".into())).await?;
        let id=failure_id(stage,&r);
        assert!(writer.replay_accounting_failure(&id,"tenant-b").await.is_err());
        assert_eq!(writer.replay_accounting_failure(&id,"tenant-a").await?,StageStatus::Applied);
        assert_eq!(writer.replay_accounting_failure(&id,"tenant-a").await?,StageStatus::Skipped);
        let totals:(i64,i64)=sqlx::query_as("SELECT current_daily_tokens,current_daily_requests FROM user_quotas").fetch_one(&pool).await?;assert_eq!(totals,(10,1));
        let counts:(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE status='accounting_applied'),count(*) FILTER(WHERE status='reconciliation_replayed') FROM billing_events").fetch_one(&pool).await?;assert_eq!(counts,(1,1));
        Ok(())
    }).await
}
#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; atomic rollback/replay acceptance unrun"]
async fn aborted_accounting_mutation_rolls_back_receipt_and_unknown_commit_remains_quarantined()
-> Result<()> {
    use ai_platform_gateway_foundation::usage_recorder::{
        AccountingStage, PgUsagePersistence, UsagePersistence, UsageRecorderError,
        stage_receipts::failure_id,
    };
    isolated(|pool,_|async move {
        accounting_fixture(&pool).await?;
        let writer=PgUsagePersistence::new(pool.clone());let r=accounting_usage();let stage=AccountingStage::QuotaCounters;
        sqlx::query("ALTER TABLE user_quotas RENAME COLUMN current_daily_tokens TO unavailable_tokens").execute(&pool).await?;
        let error=writer.post_write(stage,&[r.clone()]).await.unwrap_err();
        assert!(matches!(error,UsageRecorderError::Database(_)));
        writer.reconcile(stage,&[r.clone()],&error).await?;
        writer.reconcile(stage,&[r.clone()],&error).await?;
        let counts:(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE status='accounting_applied'),count(*) FILTER(WHERE status='reconciliation_required') FROM billing_events").fetch_one(&pool).await?;assert_eq!(counts,(0,1));
        sqlx::query("ALTER TABLE user_quotas RENAME COLUMN unavailable_tokens TO current_daily_tokens").execute(&pool).await?;
        writer.replay_accounting_failure(&failure_id(stage,&r),"tenant-a").await?;
        let tokens:i64=sqlx::query_scalar("SELECT current_daily_tokens FROM user_quotas").fetch_one(&pool).await?;assert_eq!(tokens,10);
        let mut uncertain=r.clone();uncertain.request_id="unknown-commit".into();
        writer.reconcile(stage,&[uncertain.clone()],&UsageRecorderError::CommitUncertain(sqlx::Error::PoolClosed)).await?;
        assert!(writer.replay_accounting_failure(&failure_id(stage,&uncertain),"tenant-a").await.is_err());
        let tokens:i64=sqlx::query_scalar("SELECT current_daily_tokens FROM user_quotas").fetch_one(&pool).await?;assert_eq!(tokens,10);
        Ok(())
    }).await
}

#[tokio::test]
#[ignore = "requires explicit disposable loopback PostgreSQL and Redis; quarantine conflict acceptance unrun"]
async fn accounting_failure_conflict_monotonically_quarantines_uncertain_commit() -> Result<()> {
    use ai_platform_gateway_foundation::usage_recorder::{
        AccountingStage, PgUsagePersistence, UsagePersistence, UsageRecorderError,
        stage_receipts::failure_id,
    };
    isolated(|pool, _| async move {
        accounting_fixture(&pool).await?;
        let writer = PgUsagePersistence::new(pool.clone());
        let record = accounting_usage();
        let stage = AccountingStage::QuotaCounters;
        let id = failure_id(stage, &record);
        let safe = UsageRecorderError::Collaborator("definitively aborted fixture".into());
        writer.reconcile(stage, &[record.clone()], &safe).await?;
        let original: Value =
            sqlx::query_scalar("SELECT metadata FROM billing_events WHERE event_id=$1")
                .bind(&id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(original["commit_unknown"], false);
        assert_eq!(original["replay_safe"], true);
        // With the old ON CONFLICT DO NOTHING this call left the two flags above
        // unchanged. The following assertions reproduce that observable defect.
        writer
            .reconcile(
                stage,
                &[record.clone()],
                &UsageRecorderError::CommitUncertain(sqlx::Error::PoolClosed),
            )
            .await?;
        writer.reconcile(stage, &[record.clone()], &safe).await?;
        let (status, metadata): (String, Value) =
            sqlx::query_as("SELECT status,metadata FROM billing_events WHERE event_id=$1")
                .bind(&id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(status, "reconciliation_required");
        assert_eq!(metadata["commit_unknown"], true);
        assert_eq!(metadata["replay_safe"], false);
        assert_eq!(metadata["record"], original["record"]);
        assert!(
            writer
                .replay_accounting_failure(&id, "tenant-a")
                .await
                .is_err()
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_events")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 1);
        let totals: (i64, i64) =
            sqlx::query_as("SELECT current_daily_tokens,current_daily_requests FROM user_quotas")
                .fetch_one(&pool)
                .await?;
        assert_eq!(totals, (0, 0));
        Ok(())
    })
    .await
}
