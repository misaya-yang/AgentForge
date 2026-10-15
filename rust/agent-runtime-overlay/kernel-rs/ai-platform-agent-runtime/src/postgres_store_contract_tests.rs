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
use super::http_service::RuntimeHttpService;
use super::test_support::run_with_agent_stack;

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
