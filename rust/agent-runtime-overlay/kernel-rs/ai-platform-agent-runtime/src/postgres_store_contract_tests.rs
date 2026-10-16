use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use codex_app_server_client::InProcessClientStartArgs;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartResponse;
use codex_arg0::Arg0DispatchPaths;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_core::config::ConfigBuilder;
use codex_exec_server::EnvironmentManager;
use codex_feedback::CodexFeedback;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ThreadStore;
use sha2::{Digest, Sha256};
use sqlx::AssertSqlSafe;
use sqlx::Connection;
use sqlx::PgConnection;
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;
use tempfile::TempDir;
use uuid::Uuid;

use super::AgentKernel;
use super::AiPlatformExtensionRegistry;
use super::AssistantTurnEventV1;
use super::PostgresThreadStore;
use super::postgres_store::{PlatformThreadIdentity, RuntimeStoreReadinessStage};
use super::postgres_store::execution::ExecutionClaim;
use super::http_service::RuntimeHttpService;
use super::test_support::run_with_agent_stack;

#[test]
#[ignore = "requires local PostgreSQL and AI_PLATFORM_RUNTIME_MIGRATION_PATH"]
#[allow(clippy::disallowed_methods)]
fn runtime_readiness_rejects_missing_projection_insert_privilege() {
    run_with_agent_stack("ai-platform-postgres-contract", async {
        let migration_path = std::env::var("AI_PLATFORM_RUNTIME_MIGRATION_PATH")
            .map(PathBuf::from)
            .expect("AI_PLATFORM_RUNTIME_MIGRATION_PATH must be set");
        let options = postgres_options();
        let mut admin = PgConnection::connect_with(&options)
            .await
            .expect("PostgreSQL should be reachable");
        let schema = format!("codex_runtime_readiness_{}", Uuid::new_v4().simple());
        let create_schema_sql = format!(r#"CREATE SCHEMA "{schema}""#);
        sqlx::raw_sql(AssertSqlSafe(create_schema_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("isolated schema should be created");
        let scoped_options = options.options([("search_path", format!(r#""{schema}",public"#))]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(scoped_options)
            .await
            .expect("scoped PostgreSQL pool should connect");
        prepare_schema(&pool, &migration_path).await;

        let role = format!("runtime_readiness_{}", Uuid::new_v4().simple());
        let create_role_sql = format!(
            r#"CREATE ROLE "{role}" NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT"#
        );
        sqlx::raw_sql(AssertSqlSafe(create_role_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("fixture role should be created");
        let grant_sql = format!(
            r#"
            GRANT USAGE ON SCHEMA "{schema}" TO "{role}";
            GRANT SELECT, INSERT, UPDATE ON TABLE "{schema}".assistant_runtime_threads TO "{role}";
            GRANT SELECT, INSERT ON TABLE "{schema}".assistant_runtime_thread_members TO "{role}";
            GRANT SELECT, UPDATE ON TABLE "{schema}".assistant_runtime_thread_projections TO "{role}";
            GRANT SELECT ON TABLE "{schema}".assistant_runtime_items TO "{role}";
            GRANT EXECUTE ON FUNCTION "{schema}".append_assistant_runtime_item(
                uuid, uuid, character varying, character varying, character varying,
                uuid, character varying, character varying, character varying,
                character varying, character varying, character varying, jsonb, character
            ) TO "{role}";
            "#
        );
        sqlx::raw_sql(AssertSqlSafe(grant_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("fixture role should receive the Runtime baseline privileges");

        let store = PostgresThreadStore::from_pool(pool.clone());
        let mut transaction = pool.begin().await.expect("fixture transaction should begin");
        let set_role_sql = format!("SET LOCAL ROLE \"{role}\"");
        sqlx::raw_sql(AssertSqlSafe(set_role_sql.as_str()))
            .execute(&mut *transaction)
            .await
            .expect("fixture role should be selected");
        assert_eq!(
            store.readiness_stage_on(&mut *transaction).await,
            RuntimeStoreReadinessStage::ThreadProjectionInsertMissing,
        );
        transaction.rollback().await.expect("fixture transaction should roll back");

        let grant_projection_insert_sql = format!(
            r#"GRANT INSERT ON TABLE "{schema}".assistant_runtime_thread_projections TO "{role}""#
        );
        sqlx::raw_sql(AssertSqlSafe(grant_projection_insert_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("fixture should add the missing projection grant");
        let mut transaction = pool.begin().await.expect("second transaction should begin");
        sqlx::raw_sql(AssertSqlSafe(set_role_sql.as_str()))
            .execute(&mut *transaction)
            .await
            .expect("fixture role should be selected again");
        assert_eq!(
            store.readiness_stage_on(&mut *transaction).await,
            RuntimeStoreReadinessStage::Healthy,
        );
        transaction.rollback().await.expect("second transaction should roll back");

        pool.close().await;
        let drop_schema_sql = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::raw_sql(AssertSqlSafe(drop_schema_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("isolated schema should be removed");
        let drop_role_sql = format!(r#"DROP ROLE "{role}""#);
        sqlx::raw_sql(AssertSqlSafe(drop_role_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("fixture role should be removed");
    });
}

#[test]
#[ignore = "requires local PostgreSQL and AI_PLATFORM_RUNTIME_MIGRATION_PATH"]
#[allow(clippy::disallowed_methods)]
fn agent_thread_start_round_trips_through_postgres_store() {
    run_with_agent_stack("ai-platform-postgres-contract", async {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let migration_path = std::env::var("AI_PLATFORM_RUNTIME_MIGRATION_PATH")
            .map(PathBuf::from)
            .expect("AI_PLATFORM_RUNTIME_MIGRATION_PATH must be set");
        let options = postgres_options();
        let mut admin = PgConnection::connect_with(&options)
            .await
            .expect("PostgreSQL should be reachable");
        let schema = format!("codex_runtime_rust_test_{}", Uuid::new_v4().simple());
        let create_schema_sql = format!(r#"CREATE SCHEMA "{schema}""#);
        sqlx::raw_sql(AssertSqlSafe(create_schema_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("isolated schema should be created");
        let scoped_options = options.options([("search_path", format!(r#""{schema}",public"#))]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(scoped_options)
            .await
            .expect("scoped PostgreSQL pool should connect");

        prepare_schema(&pool, &migration_path).await;
        let store = Arc::new(PostgresThreadStore::from_pool(pool.clone()));
        let session_id = format!("session-{}", Uuid::new_v4());
        let tenant_id = "tenant-rust-contract";
        let user_id = "user-rust-contract";
        sqlx::query(
        "INSERT INTO sessions (session_id, service_id, user_id, tenant_id) VALUES ($1, '__builtin_assistant__', $2, $3)",
    )
    .bind(&session_id)
    .bind(user_id)
    .bind(tenant_id)
    .execute(&pool)
    .await
    .expect("platform session should be seeded");
        let agent_home = TempDir::new().expect("temp Agent home");
        let kernel = start_test_kernel(
            store.clone(),
            agent_home.path(),
            "ai-platform-postgres-contract",
        )
        .await;
        let internal_token = format!("runtime-test-{}", Uuid::new_v4());
        let runtime = RuntimeHttpService::start(kernel, store.clone(), internal_token.clone())
            .expect("HTTP runtime should start");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("HTTP listener should bind");
        let address = listener.local_addr().expect("listener address");
        let (server_shutdown_tx, server_shutdown_rx) = tokio::sync::oneshot::channel();
        let router = runtime.router();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = server_shutdown_rx.await;
                })
                .await
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("loopback HTTP client should build");
        let create_url = format!("http://{address}/internal/v1/threads");
        let body = serde_json::json!({
            "tenantId": tenant_id,
            "userId": user_id,
            "sessionId": session_id,
            "start": {},
            "toolPolicy": {"allowedTools": []},
        });
        let unauthorized = client
            .post(&create_url)
            .json(&body)
            .send()
            .await
            .expect("unauthorized HTTP request should complete");
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
        let response = client
            .post(&create_url)
            .header("x-ai-platform-internal-token", &internal_token)
            .json(&body)
            .send()
            .await
            .expect("authorized thread/start HTTP request should complete");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let started: ThreadStartResponse = response
            .json()
            .await
            .expect("thread/start response should decode");
        let root_thread_id =
            ThreadId::from_string(&started.thread.id).expect("runtime returned a valid Thread ID");

        let history = ThreadStore::load_history(
            store.as_ref(),
            LoadThreadHistoryParams {
                thread_id: root_thread_id,
                include_archived: false,
            },
        )
        .await
        .expect("PostgreSQL history should be readable");
        assert!(!history.items.is_empty());
        // A completed turn can leave an expired claim cached. Reading the
        // immutable ceiling must not require that old owner's write fence.
        let old_claim = crate::postgres_store::execution::ExecutionClaim {
            run_id: Uuid::new_v4(),
            thread_id: root_thread_id,
            owner_id: Uuid::new_v4(),
            fence: 1,
            context: serde_json::json!({}),
        };
        store.execution_claims.lock().unwrap().insert(root_thread_id, old_claim);
        store.resolve_tool_policy(root_thread_id, None).await
            .expect("persisted ceiling remains readable with an obsolete claim");
        // The early fixture has no execution-fence function: this rejects a
        // write exactly where production rejects an expired owner. Do not
        // remove or relax the write check to make the read pass.
        let mut tx = pool.begin().await.expect("fence probe transaction");
        assert!(store.check_execution_write(
            &mut tx, Uuid::parse_str(&root_thread_id.to_string()).unwrap()
        ).await.is_err());
        tx.rollback().await.expect("rollback rejected write probe");
        store.execution_claims.lock().unwrap().remove(&root_thread_id);
        let event_id = Uuid::now_v7();
        let v1_event = AssistantTurnEventV1 {
            schema_version: "assistant-turn-contract/v1".to_string(),
            event_type: "run_started".to_string(),
            data: serde_json::json!({
                "run_id": "turn-contract",
                "session_id": session_id,
                "thread_id": root_thread_id.to_string(),
                "status": "running",
            }),
            timestamp: 1.0,
        };
        let first_sequence = store
            .append_v1_event(
                root_thread_id,
                event_id,
                "compat/turn-contract/run-started",
                &v1_event,
            )
            .await
            .expect("V1 event should append");
        let replay_sequence = store
            .append_v1_event(
                root_thread_id,
                event_id,
                "compat/turn-contract/run-started",
                &v1_event,
            )
            .await
            .expect("identical V1 replay should be idempotent");
        assert_eq!(first_sequence, replay_sequence);
        let replay = store
            .read_v1_events_after(root_thread_id, first_sequence - 1, 10)
            .await
            .expect("V1 cursor should be readable");
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].sequence, first_sequence);
        assert_eq!(replay[0].event, v1_event);
        let events_url = format!(
            "http://{address}/internal/v1/threads/{root_thread_id}/events?after_sequence={}",
            first_sequence - 1
        );
        let mut sse = client
            .get(events_url)
            .header("x-ai-platform-internal-token", &internal_token)
            .header("x-ai-tenant-id", tenant_id)
            .header("x-ai-user-id", user_id)
            .header("x-ai-session-id", &session_id)
            .send()
            .await
            .expect("SSE cursor request should connect");
        assert_eq!(sse.status(), reqwest::StatusCode::OK);
        let first_chunk = tokio::time::timeout(std::time::Duration::from_secs(2), sse.chunk())
            .await
            .expect("SSE cursor should produce a persisted event")
            .expect("SSE body should remain readable")
            .expect("SSE cursor should not close before the first event");
        let first_chunk = String::from_utf8(first_chunk.to_vec()).expect("SSE is UTF-8");
        assert!(first_chunk.contains("event: run_started"));
        assert!(first_chunk.contains(&format!("id: {first_sequence}")));
        drop(sse);
        let member_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM assistant_runtime_thread_members WHERE runtime_thread_id = $1",
        )
        .bind(Uuid::parse_str(&root_thread_id.to_string()).expect("thread id is a UUID"))
        .fetch_one(&pool)
        .await
        .expect("member mapping should be queryable");
        assert_eq!(member_count, 1);

        let _ = server_shutdown_tx.send(());
        server
            .await
            .expect("HTTP server task should join")
            .expect("HTTP server should stop cleanly");
        runtime.shutdown().await;

        let resumed_agent_home = TempDir::new().expect("second-process Agent home");
        let resumed_kernel = start_test_kernel(
            store.clone(),
            resumed_agent_home.path(),
            "ai-platform-postgres-resume-contract",
        )
        .await;
        let resumed_runtime =
            RuntimeHttpService::start(resumed_kernel, store.clone(), internal_token.clone())
                .expect("second HTTP runtime should start");
        let resumed_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("second HTTP listener should bind");
        let resumed_address = resumed_listener
            .local_addr()
            .expect("second listener address");
        let (resumed_shutdown_tx, resumed_shutdown_rx) = tokio::sync::oneshot::channel();
        let resumed_router = resumed_runtime.router();
        let resumed_server = tokio::spawn(async move {
            axum::serve(resumed_listener, resumed_router)
                .with_graceful_shutdown(async {
                    let _ = resumed_shutdown_rx.await;
                })
                .await
        });
        let lifecycle_url =
            format!("http://{resumed_address}/internal/v1/threads/{root_thread_id}");
        let resumed_response = scoped_post(
            &client,
            format!("{lifecycle_url}/resume"),
            &internal_token,
            tenant_id,
            user_id,
            &session_id,
        )
        .send()
        .await
        .expect("thread/resume HTTP request should complete");
        assert_eq!(resumed_response.status(), reqwest::StatusCode::OK);
        let resumed: ThreadResumeResponse = resumed_response
            .json()
            .await
            .expect("thread/resume response should decode");
        assert_eq!(resumed.thread.id, root_thread_id.to_string());

        let archived = scoped_post(
            &client,
            format!("{lifecycle_url}/archive"),
            &internal_token,
            tenant_id,
            user_id,
            &session_id,
        )
        .send()
        .await
        .expect("thread/archive HTTP request should complete");
        assert_eq!(archived.status(), reqwest::StatusCode::OK);
        let hidden = ThreadStore::read_thread(
            store.as_ref(),
            codex_thread_store::ReadThreadParams {
                thread_id: root_thread_id,
                include_archived: false,
                include_history: false,
            },
        )
        .await;
        assert!(matches!(
            hidden,
            Err(codex_thread_store::ThreadStoreError::ThreadNotFound { .. })
        ));

        let unarchived = scoped_post(
            &client,
            format!("{lifecycle_url}/unarchive"),
            &internal_token,
            tenant_id,
            user_id,
            &session_id,
        )
        .send()
        .await
        .expect("thread/unarchive HTTP request should complete");
        assert_eq!(unarchived.status(), reqwest::StatusCode::OK);
        ThreadStore::read_thread(
            store.as_ref(),
            codex_thread_store::ReadThreadParams {
                thread_id: root_thread_id,
                include_archived: false,
                include_history: false,
            },
        )
        .await
        .expect("unarchived thread should be visible");

        let _ = resumed_shutdown_tx.send(());
        resumed_server
            .await
            .expect("second HTTP server task should join")
            .expect("second HTTP server should stop cleanly");
        resumed_runtime.shutdown().await;

        // A failed append may be swallowed by upstream Core. Once PostgreSQL
        // recovers, the accepted-input durability failure must remain visible.
        sqlx::raw_sql(AssertSqlSafe(
            "CREATE FUNCTION reject_rollout_append() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected append failure'; END $$; CREATE TRIGGER reject_rollout_append BEFORE INSERT ON assistant_runtime_items FOR EACH ROW EXECUTE FUNCTION reject_rollout_append();",
        ))
        .execute(&pool).await.expect("install isolated append failure");
        let before = ThreadStore::load_history(
            store.as_ref(),
            LoadThreadHistoryParams {
                thread_id: root_thread_id,
                include_archived: false,
            },
        )
        .await
        .expect("history before failed append");
        let failed = ThreadStore::append_items(
            store.as_ref(),
            codex_thread_store::AppendThreadItemsParams {
                thread_id: root_thread_id,
                items: vec![before.items[0].clone()],
            },
        )
        .await;
        assert!(failed.is_err());
        sqlx::query("DROP TRIGGER reject_rollout_append ON assistant_runtime_items")
            .execute(&pool)
            .await
            .expect("recover PostgreSQL writes");
        let after = ThreadStore::load_history(
            store.as_ref(),
            LoadThreadHistoryParams {
                thread_id: root_thread_id,
                include_archived: false,
            },
        )
        .await
        .expect("healthy database reads resume");
        assert_eq!(before.items.len(), after.items.len());
        for checkpoint in [
            codex_thread_store::PersistContext::ThreadPreparation,
            codex_thread_store::PersistContext::Standard,
            codex_thread_store::PersistContext::TurnStart,
            codex_thread_store::PersistContext::SteeredUserInput,
        ] {
            assert!(
                ThreadStore::persist_thread(store.as_ref(), root_thread_id, checkpoint)
                    .await
                    .is_err()
            );
        }
        assert!(
            store
                .admit_turn_terminal(root_thread_id, "after-final-model")
                .await
                .is_err()
        );
        assert!(
            ThreadStore::flush_thread(store.as_ref(), root_thread_id)
                .await
                .is_err()
        );
        assert!(
            ThreadStore::shutdown_thread(store.as_ref(), root_thread_id)
                .await
                .is_err()
        );
        pool.close().await;
        let drop_schema_sql = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::raw_sql(AssertSqlSafe(drop_schema_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("isolated schema should be removed");
    });
}

#[test]
#[ignore = "requires local PostgreSQL and AI_PLATFORM_RUNTIME_MIGRATION_PATH"]
#[allow(clippy::disallowed_methods)]
fn execution_heartbeat_requires_live_snapshot_authority() {
    run_with_agent_stack("ai-platform-runtime-heartbeat-authority", async {
        let migration_path = std::env::var("AI_PLATFORM_RUNTIME_MIGRATION_PATH")
            .map(PathBuf::from)
            .expect("AI_PLATFORM_RUNTIME_MIGRATION_PATH must be set");
        let options = postgres_options();
        let mut admin = PgConnection::connect_with(&options)
            .await
            .expect("PostgreSQL should be reachable");
        let schema = format!("codex_runtime_heartbeat_{}", Uuid::new_v4().simple());
        let create_schema_sql = format!(r#"CREATE SCHEMA "{schema}""#);
        sqlx::raw_sql(AssertSqlSafe(create_schema_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("isolated schema should be created");
        let scoped_options = options.options([("search_path", format!(r#""{schema}",public"#))]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(scoped_options)
            .await
            .expect("scoped PostgreSQL pool should connect");
        prepare_schema(&pool, &migration_path).await;
        prepare_heartbeat_authority_tables(&pool).await;
        let store = PostgresThreadStore::from_pool(pool.clone());

        let valid = seed_heartbeat_run(&pool, &store, None).await;
        let valid_claim = seed_execution_claim(&pool, &valid).await;
        let valid_before = execution_owner_lease(&pool, valid.run_id).await;
        assert!(
            store
                .heartbeat_execution(&valid_claim)
                .await
                .expect("valid heartbeat should query")
        );
        let valid_after = execution_owner_lease(&pool, valid.run_id).await;
        assert!(
            valid_after > valid_before,
            "valid authority should renew its lease"
        );

        // The active model and owner leases remain valid around an already
        // expired snapshot, isolating snapshot expiry as the refused authority.
        let expiring = seed_heartbeat_run(&pool, &store, Some(-1.0)).await;
        let expiring_claim = seed_execution_claim(&pool, &expiring).await;
        let snapshot_is_expired: bool = sqlx::query_scalar(
            "SELECT expires_at<=clock_timestamp() FROM assistant_runtime_snapshots WHERE snapshot_id=$1",
        )
        .bind(expiring.snapshot_id)
        .fetch_one(&pool)
        .await
        .expect("snapshot expiry should be queryable");
        assert!(snapshot_is_expired);
        assert_active_heartbeat_authority(&pool, expiring.run_id).await;
        assert_heartbeat_does_not_renew(
            &store,
            &pool,
            &expiring_claim,
            "expired snapshot heartbeat should be refused",
        )
        .await;

        let revoked = seed_heartbeat_run(&pool, &store, None).await;
        let revoked_claim = seed_execution_claim(&pool, &revoked).await;
        sqlx::query(
            "INSERT INTO assistant_runtime_snapshot_revocations (snapshot_id,tenant_id,user_id,session_id,reason_code,revoked_by,metadata) VALUES ($1,$2,$3,$4,'fixture_revocation','runtime-heartbeat-test','{}'::jsonb)",
        )
        .bind(revoked.snapshot_id)
        .bind(&revoked.tenant_id)
        .bind(&revoked.user_id)
        .bind(&revoked.session_id)
        .execute(&pool)
        .await
        .expect("snapshot revocation should persist");
        assert_active_heartbeat_authority(&pool, revoked.run_id).await;
        assert_heartbeat_does_not_renew(
            &store,
            &pool,
            &revoked_claim,
            "revoked snapshot heartbeat should be refused",
        )
        .await;

        // Simulate a corrupt run-to-snapshot binding after a valid lease and
        // owner claim exist. This drops only the isolated fixture's FK so the
        // production heartbeat query itself must reject the mismatch.
        let mismatched = seed_heartbeat_run(&pool, &store, None).await;
        let foreign_snapshot = seed_heartbeat_run(&pool, &store, None).await;
        let mismatched_claim = seed_execution_claim(&pool, &mismatched).await;
        sqlx::query("ALTER TABLE assistant_runs DROP CONSTRAINT IF EXISTS assistant_runs_runtime_snapshot_fk")
            .execute(&pool)
            .await
            .expect("isolated fixture should permit corrupt binding probe");
        sqlx::query("UPDATE assistant_runs SET runtime_snapshot_id=$2 WHERE run_id=$1")
            .bind(mismatched.run_id)
            .bind(foreign_snapshot.snapshot_id)
            .execute(&pool)
            .await
            .expect("fixture snapshot mismatch should persist");
        assert_active_heartbeat_authority(&pool, mismatched.run_id).await;
        assert_heartbeat_does_not_renew(
            &store,
            &pool,
            &mismatched_claim,
            "mismatched snapshot heartbeat should be refused",
        )
        .await;

        pool.close().await;
        let drop_schema_sql = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::raw_sql(AssertSqlSafe(drop_schema_sql.as_str()))
            .execute(&mut admin)
            .await
            .expect("isolated schema should be removed");
    });
}

async fn prepare_heartbeat_authority_tables(pool: &PgPool) {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS assistant_runtime_execution_owners (
            run_id UUID PRIMARY KEY REFERENCES assistant_runs(run_id) ON DELETE RESTRICT,
            runtime_thread_id UUID NOT NULL REFERENCES assistant_runtime_threads(runtime_thread_id) ON DELETE RESTRICT,
            owner_id UUID NOT NULL,
            fence BIGINT NOT NULL CHECK (fence > 0),
            lease_until TIMESTAMPTZ NOT NULL,
            context JSONB NOT NULL CHECK (jsonb_typeof(context) = 'object'),
            heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
            created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
        );
        CREATE TABLE IF NOT EXISTS assistant_runtime_model_leases (
            lease_id UUID PRIMARY KEY,
            snapshot_id UUID NOT NULL,
            run_id UUID NOT NULL,
            runtime_thread_id UUID NOT NULL,
            tenant_id VARCHAR(255) NOT NULL,
            user_id VARCHAR(255) NOT NULL,
            session_id VARCHAR(255) NOT NULL,
            status VARCHAR(32) NOT NULL CHECK (status IN ('active', 'exhausted', 'revoked', 'expired')),
            expires_at TIMESTAMPTZ NOT NULL,
            FOREIGN KEY (snapshot_id,run_id,tenant_id,user_id,session_id)
                REFERENCES assistant_runtime_snapshots(snapshot_id,run_id,tenant_id,user_id,session_id)
                ON DELETE RESTRICT,
            FOREIGN KEY (run_id) REFERENCES assistant_runs(run_id) ON DELETE RESTRICT,
            FOREIGN KEY (runtime_thread_id,tenant_id,user_id,session_id)
                REFERENCES assistant_runtime_threads(runtime_thread_id,tenant_id,user_id,session_id)
                ON DELETE RESTRICT,
            UNIQUE (snapshot_id)
        );
        "#,
    )
    .execute(pool)
    .await
    .expect("heartbeat authority tables should be ready");
}

struct HeartbeatFixture {
    run_id: Uuid,
    snapshot_id: Uuid,
    runtime_thread_id: Uuid,
    tenant_id: String,
    user_id: String,
    session_id: String,
}

async fn seed_heartbeat_run(
    pool: &PgPool,
    store: &PostgresThreadStore,
    snapshot_ttl_seconds: Option<f64>,
) -> HeartbeatFixture {
    let suffix = Uuid::new_v4().simple().to_string();
    let tenant_id = format!("tenant-heartbeat-{suffix}");
    let user_id = format!("user-heartbeat-{suffix}");
    let session_id = format!("session-heartbeat-{suffix}");
    let runtime_thread_id = Uuid::new_v4();
    let runtime_thread = ThreadId::from_u128(runtime_thread_id.as_u128());
    let run_id = Uuid::new_v4();
    let snapshot_id = Uuid::new_v4();
    let lease_id = Uuid::new_v4();
    let snapshot = serde_json::json!({});
    let snapshot_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&snapshot).expect("fixture snapshot should serialize")),
    );

    sqlx::query(
        "INSERT INTO sessions (session_id,service_id,user_id,tenant_id) VALUES ($1,'__builtin_assistant__',$2,$3)",
    )
    .bind(&session_id)
    .bind(&user_id)
    .bind(&tenant_id)
    .execute(pool)
    .await
    .expect("fixture session should persist");
    sqlx::query(
        "INSERT INTO assistant_session_runtime_assignments (tenant_id,user_id,session_id,runtime_owner,kernel_revision) VALUES ($1,$2,$3,'agent_runtime','heartbeat-fixture')",
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&session_id)
    .execute(pool)
    .await
    .expect("fixture session should be assigned to Runtime");
    store
        .authorize_root(&PlatformThreadIdentity::new(
            runtime_thread,
            tenant_id.clone(),
            user_id.clone(),
            session_id.clone(),
        ))
        .await
        .expect("fixture thread scope should persist");

    let mut transaction = pool
        .begin()
        .await
        .expect("fixture transaction should begin");
    sqlx::query(
        "INSERT INTO assistant_runtime_snapshots (snapshot_id,runtime_thread_id,run_id,tenant_id,user_id,session_id,schema_version,capability_revision,snapshot,snapshot_sha256,expires_at) VALUES ($1,$2,$3,$4,$5,$6,'agent-runtime-snapshot/v1',1,$7,$8,CASE WHEN $9::double precision IS NULL THEN NULL ELSE clock_timestamp()+make_interval(secs=>$9::double precision) END)",
    )
    .bind(snapshot_id)
    .bind(runtime_thread_id)
    .bind(run_id)
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&session_id)
    .bind(snapshot)
    .bind(snapshot_sha256)
    .bind(snapshot_ttl_seconds)
    .execute(&mut *transaction)
    .await
    .expect("fixture snapshot should persist");
    sqlx::query(
        "INSERT INTO assistant_runs (run_id,tenant_id,user_id,session_id,status,engine,harness_thread_id,harness_turn_id,runtime_snapshot_id,kernel_revision,capability_revision) VALUES ($1,$2,$3,$4,'running','agent_runtime',$5,$6,$7,'heartbeat-fixture',1)",
    )
    .bind(run_id)
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&session_id)
    .bind(runtime_thread_id)
    .bind(run_id.to_string())
    .bind(snapshot_id)
    .execute(&mut *transaction)
    .await
    .expect("fixture active run should persist");
    sqlx::query(
        "INSERT INTO assistant_runtime_model_leases (lease_id,snapshot_id,run_id,runtime_thread_id,tenant_id,user_id,session_id,status,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,'active',clock_timestamp()+interval '5 minutes')",
    )
    .bind(lease_id)
    .bind(snapshot_id)
    .bind(run_id)
    .bind(runtime_thread_id)
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&session_id)
    .execute(&mut *transaction)
    .await
    .expect("fixture model lease should persist");
    transaction
        .commit()
        .await
        .expect("fixture authority should commit");

    HeartbeatFixture {
        run_id,
        snapshot_id,
        runtime_thread_id,
        tenant_id,
        user_id,
        session_id,
    }
}

async fn seed_execution_claim(pool: &PgPool, fixture: &HeartbeatFixture) -> ExecutionClaim {
    let owner_id = Uuid::new_v4();
    let claim = ExecutionClaim {
        run_id: fixture.run_id,
        thread_id: ThreadId::from_u128(fixture.runtime_thread_id.as_u128()),
        owner_id,
        fence: 1,
        context: serde_json::json!({"fixture":"heartbeat"}),
    };
    sqlx::query(
        "INSERT INTO assistant_runtime_execution_owners (run_id,runtime_thread_id,owner_id,fence,lease_until,context) VALUES ($1,$2,$3,$4,clock_timestamp()+interval '20 seconds',$5)",
    )
    .bind(claim.run_id)
    .bind(fixture.runtime_thread_id)
    .bind(claim.owner_id)
    .bind(claim.fence)
    .bind(&claim.context)
    .execute(pool)
    .await
    .expect("fixture execution owner should persist");
    claim
}

async fn execution_owner_lease(pool: &PgPool, run_id: Uuid) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar("SELECT lease_until FROM assistant_runtime_execution_owners WHERE run_id=$1")
        .bind(run_id)
        .fetch_one(pool)
        .await
        .expect("execution owner lease should be readable")
}

async fn assert_active_heartbeat_authority(pool: &PgPool, run_id: Uuid) {
    let (run_active, model_lease_active, owner_lease_active): (bool, bool, bool) = sqlx::query_as(
        "SELECT r.status IN ('running','awaiting_approval'), \
         EXISTS (SELECT 1 FROM assistant_runtime_model_leases l WHERE l.run_id=r.run_id AND l.status='active' AND l.expires_at>clock_timestamp()), \
         EXISTS (SELECT 1 FROM assistant_runtime_execution_owners o WHERE o.run_id=r.run_id AND o.lease_until>clock_timestamp()) \
         FROM assistant_runs r WHERE r.run_id=$1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .expect("run and live lease authority should be queryable");
    assert!(run_active, "run must remain active");
    assert!(model_lease_active, "model lease must remain active");
    assert!(owner_lease_active, "execution-owner lease must remain active");
}

async fn assert_heartbeat_does_not_renew(
    store: &PostgresThreadStore,
    pool: &PgPool,
    claim: &ExecutionClaim,
    reason: &str,
) {
    let before = execution_owner_lease(pool, claim.run_id).await;
    assert!(!store.heartbeat_execution(claim).await.expect(reason));
    assert_eq!(execution_owner_lease(pool, claim.run_id).await, before);
}

async fn start_test_kernel(
    store: Arc<PostgresThreadStore>,
    agent_home: &Path,
    client_name: &str,
) -> AgentKernel {
    let loader_overrides = LoaderOverrides::without_managed_config_for_tests();
    let config = Arc::new(
        ConfigBuilder::default()
            .codex_home(agent_home.to_path_buf())
            .fallback_cwd(Some(agent_home.to_path_buf()))
            .loader_overrides(loader_overrides.clone())
            .build()
            .await
            .expect("test config should build"),
    );
    AgentKernel::start(
        InProcessClientStartArgs {
            arg0_paths: Arg0DispatchPaths::default(),
            config,
            cli_overrides: Vec::new(),
            loader_overrides,
            strict_config: true,
            cloud_config_bundle: CloudConfigBundleLoader::default(),
            embedded_network_policy: Default::default(),
            feedback: CodexFeedback::new(),
            log_db: None,
            state_db: None,
            environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
            config_warnings: Vec::new(),
            session_source: SessionSource::Cli,
            enable_codex_api_key_env: false,
            client_name: client_name.to_string(),
            client_version: "0.0.0-test".to_string(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 8,
        },
        store,
        AiPlatformExtensionRegistry::new(),
    )
    .await
    .expect("Agent kernel should start")
}

fn scoped_post(
    client: &reqwest::Client,
    url: String,
    internal_token: &str,
    tenant_id: &str,
    user_id: &str,
    session_id: &str,
) -> reqwest::RequestBuilder {
    client
        .post(url)
        .header("x-ai-platform-internal-token", internal_token)
        .header("x-ai-tenant-id", tenant_id)
        .header("x-ai-user-id", user_id)
        .header("x-ai-session-id", session_id)
        .json(&serde_json::json!({}))
}

fn postgres_options() -> PgConnectOptions {
    let port = std::env::var("POSTGRES_PORT")
        .expect("POSTGRES_PORT must be set")
        .parse()
        .expect("POSTGRES_PORT must be numeric");
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username(&std::env::var("POSTGRES_USER").expect("POSTGRES_USER must be set"))
        .password(&std::env::var("POSTGRES_PASSWORD").expect("POSTGRES_PASSWORD must be set"))
        .database(&std::env::var("POSTGRES_DB").expect("POSTGRES_DB must be set"))
}

async fn prepare_schema(pool: &PgPool, migration_path: &PathBuf) {
    sqlx::raw_sql(
        r#"
        CREATE TABLE sessions (
            session_id VARCHAR(255) PRIMARY KEY,
            service_id VARCHAR(255), user_id VARCHAR(255), tenant_id VARCHAR(255),
            state JSONB NOT NULL DEFAULT '{}'::jsonb,
            history JSONB NOT NULL DEFAULT '[]'::jsonb,
            metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
            config JSONB NOT NULL DEFAULT '{}'::jsonb,
            status VARCHAR(50) NOT NULL DEFAULT 'active',
            expires_at TIMESTAMPTZ, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );
        CREATE TABLE assistant_runs (
            run_id UUID PRIMARY KEY, tenant_id VARCHAR(255) NOT NULL,
            user_id VARCHAR(255) NOT NULL, session_id VARCHAR(255) NOT NULL,
            status VARCHAR(32) NOT NULL DEFAULT 'running',
            engine VARCHAR(32) NOT NULL DEFAULT 'agent_loop',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );
        CREATE OR REPLACE FUNCTION update_assistant_gateway_timestamp()
        RETURNS TRIGGER AS $$ BEGIN NEW.updated_at = NOW(); RETURN NEW; END; $$ LANGUAGE plpgsql;
        "#,
    )
    .execute(pool)
    .await
    .expect("base schema should be created");
    let migration = std::fs::read_to_string(migration_path)
        .expect("runtime migration should be readable from the main repository");
    sqlx::raw_sql(AssertSqlSafe(migration.as_str()))
        .execute(pool)
        .await
        .expect("runtime migration should apply");
    sqlx::raw_sql(AssertSqlSafe(migration.as_str()))
        .execute(pool)
        .await
        .expect("runtime migration should be idempotent");
}
