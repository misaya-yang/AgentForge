//! Explicit isolated PostgreSQL regression target. Only session-local tables are
//! created; the supplied database's persistent tables are never read or changed.
use super::*;
use crate::{KnowledgeBackends, KnowledgeConnections, KnowledgeGraph, process::ProcessSettings};
use axum::{extract::State, routing::post};
use std::sync::atomic::{AtomicBool, Ordering};

struct Fixture {
    sources: Arc<Sources>,
    pool: PgPool,
    actor: query::types::UserContext,
    actor_active: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
    _graph: KnowledgeGraph,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("KNOWLEDGE_TEST_DATABASE_URL")
            .expect("explicit isolated PostgreSQL fixture URL required");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(&url)
            .await
            .unwrap();
        // A single retained session, with no persistent-schema fallback.
        sqlx::raw_sql("SET search_path TO pg_temp,pg_catalog;
            CREATE TEMP TABLE datasets(dataset_id text PRIMARY KEY, tenant_id text, created_by text, visibility text DEFAULT 'private', is_deleted boolean DEFAULT false, is_archived boolean DEFAULT false, index_config jsonb DEFAULT '{}');
            CREATE TEMP TABLE dataset_permissions(dataset_id text,subject_type text,subject_id text,permission text);
            CREATE TEMP TABLE confluence_connections(connection_id text PRIMARY KEY,tenant_id text,owner_id text,created_by text,name text,domain text,status text,sync_mode text,email text,api_token text,updated_at timestamptz);
            CREATE TEMP TABLE confluence_space_bindings(binding_id text PRIMARY KEY,dataset_id text,connection_id text,tenant_id text,owner_id text,created_by text);
            CREATE TEMP TABLE confluence_sync_tasks(task_id text PRIMARY KEY,binding_id text,owner_id text,status text);
            INSERT INTO datasets(dataset_id,tenant_id,created_by) VALUES('dataset','tenant','owner');
            INSERT INTO confluence_connections VALUES('connection','tenant','owner','owner','Cloud','example.atlassian.net','active','manual','fixture@example.invalid','synthetic-token','2020-01-01T00:00:00Z');
            INSERT INTO confluence_space_bindings VALUES('binding','dataset','connection','tenant','owner','owner');
            INSERT INTO confluence_sync_tasks VALUES('task','binding','owner','processing');")
            .execute(&pool).await.unwrap();
        let actor_active = Arc::new(AtomicBool::new(true));
        let router = Router::new().route(query::actor::ACTOR_PATH, post(
            |State(active): State<Arc<AtomicBool>>| async move {
                if !active.load(Ordering::Acquire) {
                    return StatusCode::FORBIDDEN.into_response();
                }
                Json(json!({"tenant_id":"tenant","user_id":"owner","status":"active","tier":"normal","roles":["user"]})).into_response()
            })).with_state(actor_active.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let settings = ProcessSettings::from_values(
            std::collections::BTreeMap::from([
                ("ENVIRONMENT".into(), "test".into()),
                (
                    "AI_PLATFORM_INTERNAL_TOKEN".into(),
                    "offline-confluence-signing-material".into(),
                ),
                (
                    "AI_PLATFORM_CAPABILITY_PROOF_SECRET".into(),
                    "offline-confluence-proof-material".into(),
                ),
                ("INTERNAL_COMM_STATE_BACKEND".into(), "memory".into()),
                ("INTERNAL_IDEMPOTENCY_BACKEND".into(), "memory".into()),
                ("KNOWLEDGE_RUNTIME_ROLE".into(), "api".into()),
                ("GATEWAY_URL".into(), origin),
                ("KNOWLEDGE_CONNECTOR_ENCRYPTION_KEY".into(), "synthetic-key".into()),
            ]),
            std::path::PathBuf::from("/tmp/knowledge-confluence-parity"),
        )
        .unwrap();
        let graph = KnowledgeGraph::new(
            KnowledgeConnections {
                model_plane:Some(crate::model_plane_identity::no_network_test_client()),
                database: pool.clone(),
                replay_redis: None,
                idempotency_redis: None,
            },
            settings.configuration,
            KnowledgeBackends::default(),
        )
        .unwrap();
        let sources = graph.application.as_ref().unwrap().sources.clone();
        let actor = serde_json::from_value(json!({"tenant_id":"tenant","user_id":"owner","user_tier":"normal","user_type":"user","roles":["user"],"is_authenticated":true})).unwrap();
        Self {
            sources,
            pool,
            actor,
            actor_active,
            server,
            _graph: graph,
        }
    }
    async fn row(&self) -> Value {
        self.sources
            .connection("connection")
            .await
            .unwrap()
            .unwrap()
    }
    async fn command(&self, status: &str) -> ControlConnection {
        let row = self.row().await;
        ControlConnection {
            expected_revision: revision(&row),
            expected_status: text(&row, "status").into(),
            status: status.into(),
        }
    }
    async fn control(&self, command: ControlConnection) -> Result<Value> {
        self.sources
            .control(&self.actor, "dataset", "connection", command)
            .await
    }
    async fn execution(&self) -> super::super::sync::Execution {
        super::super::sync::Execution {
            binding: self.sources.binding("binding").await.unwrap().unwrap(),
            connection: self.row().await,
            actor: self.actor.clone(),
            task_id: "task".into(),
        }
    }
    async fn assert_both_guards_refuse(&self, execution: &super::super::sync::Execution) {
        assert!(self.sources.guard(execution).await.is_err());
        let mut connection = self.pool.acquire().await.unwrap();
        assert!(
            self.sources
                .guard_on(execution, &mut connection)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
#[ignore = "requires explicit isolated KNOWLEDGE_TEST_DATABASE_URL; creates session-local tables only"]
async fn connection_control_exact_cas_preserves_credentials_and_readback() {
    let f = Fixture::new().await;
    for encrypted in [false, true] {
        sqlx::query("UPDATE confluence_connections SET status='active',updated_at='2020-01-01T00:00:00Z',email='fixture@example.invalid',api_token='synthetic-token'").execute(&f.pool).await.unwrap();
        if encrypted {
            let sealed = credentials::seal(
                &f.row().await,
                "fixture@example.invalid",
                "synthetic-token",
                &f.sources.encryption_key,
            )
            .unwrap();
            sqlx::query("UPDATE confluence_connections SET email=$1,api_token=$2")
                .bind(credentials::MARKER)
                .bind(sealed)
                .execute(&f.pool)
                .await
                .unwrap();
        }
        let before = f.row().await;
        let stale = f.command("disabled").await;
        let duplicate = f.command("disabled").await;
        let paused = f.control(stale).await.unwrap();
        assert_eq!(paused["status"], "disabled");
        assert_eq!(
            f.control(duplicate).await.unwrap_err().status,
            StatusCode::CONFLICT
        );
        // Discard the successful acknowledgement, then use the production
        // metadata readback. This does not simulate a PostgreSQL COMMIT fault.
        drop(paused);
        let readback = f
            .sources
            .connection_status(&f.actor, "dataset", "connection")
            .await
            .unwrap();
        assert_eq!(readback["status"], "disabled");
        assert!(readback.get("api_token").is_none() && readback.get("email").is_none());
        let resumed = f.control(f.command("active").await).await.unwrap();
        assert_ne!(resumed["revision"], revision(&before));
        let mut after = f.row().await;
        assert_eq!(
            credentials::open(&after, &f.sources.encryption_key)
                .unwrap()
                .token,
            "synthetic-token"
        );
        after["updated_at"] = before["updated_at"].clone();
        assert_eq!(after, before);
        let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM confluence_sync_tasks")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(tasks, 1);
    }
}

#[tokio::test]
#[ignore = "requires explicit isolated KNOWLEDGE_TEST_DATABASE_URL; creates session-local tables only"]
async fn pause_fast_resume_and_replacement_invalidate_admitted_task() {
    let f = Fixture::new().await;
    let execution = f.execution().await;
    assert!(f.sources.guard(&execution).await.is_ok());
    f.control(f.command("disabled").await).await.unwrap();
    f.assert_both_guards_refuse(&execution).await;
    f.control(f.command("active").await).await.unwrap();
    f.assert_both_guards_refuse(&execution).await;
    let resumed_execution = f.execution().await;
    assert!(f.sources.guard(&resumed_execution).await.is_ok());
    let before = f.row().await;
    f.sources
        .change_connection(
            &f.actor,
            "dataset",
            "connection",
            &revision(&before),
            "active",
            None,
            Some(("replacement@example.invalid", "replacement-token")),
        )
        .await
        .unwrap();
    f.assert_both_guards_refuse(&resumed_execution).await;
    let after = f.row().await;
    assert_eq!(after["status"], "active");
    assert_eq!(
        credentials::open(&after, &f.sources.encryption_key)
            .unwrap()
            .token,
        "replacement-token"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM confluence_sync_tasks WHERE task_id='task'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(status, "processing");
}

#[tokio::test]
#[ignore = "requires explicit isolated KNOWLEDGE_TEST_DATABASE_URL; creates session-local tables only"]
async fn controls_and_publication_guards_recheck_revocation_and_monotonic_clock() {
    let f = Fixture::new().await;
    let execution = f.execution().await;
    for mutation in [
        "UPDATE confluence_connections SET owner_id='other'",
        "UPDATE confluence_connections SET tenant_id='other'",
        "UPDATE datasets SET created_by='other'",
        "UPDATE datasets SET is_archived=true",
        "UPDATE datasets SET is_deleted=true",
    ] {
        let command = f.command("disabled").await;
        sqlx::query(mutation).execute(&f.pool).await.unwrap();
        assert!(f.control(command).await.is_err());
        f.assert_both_guards_refuse(&execution).await;
        assert_eq!(f.row().await["status"], "active");
        sqlx::raw_sql("UPDATE confluence_connections SET owner_id='owner',tenant_id='tenant'; UPDATE datasets SET created_by='owner',is_archived=false,is_deleted=false;").execute(&f.pool).await.unwrap();
    }
    f.actor_active.store(false, Ordering::Release);
    assert!(f.control(f.command("disabled").await).await.is_err());
    f.assert_both_guards_refuse(&execution).await;
    f.actor_active.store(true, Ordering::Release);
    sqlx::query("UPDATE confluence_connections SET updated_at=NOW()+INTERVAL '1 day'")
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.control(f.command("disabled").await)
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(f.row().await["status"], "active");
    // Controls/readback remain available while content publication is fenced.
    sqlx::raw_sql("UPDATE confluence_connections SET updated_at='2020-01-01T00:00:00Z'; UPDATE datasets SET index_config='{\"retrieval\":{\"_index_deletion_fence\":{\"operation\":\"dataset_delete\"}}}';").execute(&f.pool).await.unwrap();
    assert!(f.sources.guard(&execution).await.is_err());
    assert_eq!(
        f.control(f.command("disabled").await).await.unwrap()["status"],
        "disabled"
    );
    assert_eq!(
        f.sources
            .connection_status(&f.actor, "dataset", "connection")
            .await
            .unwrap()["status"],
        "disabled"
    );
}

#[tokio::test]
#[ignore = "requires explicit isolated KNOWLEDGE_TEST_DATABASE_URL; creates session-local tables only"]
async fn guards_reject_binding_task_and_editor_grant_revocation() {
    let f = Fixture::new().await;
    let execution = f.execution().await;
    for mutation in [
        "UPDATE confluence_space_bindings SET owner_id='other'",
        "UPDATE confluence_space_bindings SET dataset_id='other'",
        "UPDATE confluence_space_bindings SET connection_id='other'",
        "UPDATE confluence_sync_tasks SET owner_id='other'",
        "UPDATE confluence_sync_tasks SET status='completed'",
    ] {
        sqlx::query(mutation).execute(&f.pool).await.unwrap();
        f.assert_both_guards_refuse(&execution).await;
        sqlx::raw_sql("UPDATE confluence_space_bindings SET owner_id='owner',dataset_id='dataset',connection_id='connection'; UPDATE confluence_sync_tasks SET owner_id='owner',status='processing';").execute(&f.pool).await.unwrap();
    }
    sqlx::raw_sql("UPDATE datasets SET created_by='someone-else'; INSERT INTO dataset_permissions VALUES('dataset','user','owner','editor');").execute(&f.pool).await.unwrap();
    assert!(f.sources.guard(&execution).await.is_ok());
    {
        let mut connection = f.pool.acquire().await.unwrap();
        assert!(
            f.sources
                .guard_on(&execution, &mut connection)
                .await
                .is_ok()
        );
    }
    sqlx::query("DELETE FROM dataset_permissions")
        .execute(&f.pool)
        .await
        .unwrap();
    f.assert_both_guards_refuse(&execution).await;
    assert!(f.control(f.command("disabled").await).await.is_err());
    assert!(
        f.sources
            .connection_status(&f.actor, "dataset", "connection")
            .await
            .is_err()
    );
}
