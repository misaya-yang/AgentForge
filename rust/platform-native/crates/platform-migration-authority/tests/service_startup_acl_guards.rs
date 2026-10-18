//! Real PostgreSQL ACL semantics: each required permission and forbidden identity
//! write is independently exercised, inside one rolled-back isolated transaction.
use ai_platform_migration_authority::{credentials, source_tools::measure_successors};
use serde_json::Value;
use sqlx::{Connection, Executor};
use std::{net::IpAddr, path::PathBuf};

const CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../docs/design/proposals/service-successors/catalog.json"
));
fn contracts() -> Value {
    serde_json::from_str(CATALOG).unwrap()
}
#[test]
fn service_guards_consume_authority_contract_and_keep_new_outbound_fields_closed() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    for (path, key, start, end) in [
        (
            "rust/platform-native/crates/platform-gateway/src/migration_host/control_plane/a2a.rs",
            "gateway_a2a",
            "pub async fn verify_schema(",
            "fn task_state(",
        ),
        (
            "rust/platform-native/crates/platform-knowledge-domain/src/migration_knowledge_ingest/worker.rs",
            "knowledge_worker",
            "pub async fn validate_storage(",
            "pub async fn queue_depth(",
        ),
    ] {
        let source = std::fs::read_to_string(root.join(path)).unwrap();
        let guard = source
            .split_once(start)
            .unwrap()
            .1
            .split_once(end)
            .unwrap()
            .0;
        assert!(guard.contains("/docs/design/proposals/service-successors/catalog.json"));
        assert!(guard.contains(&format!("[\"admission_requirements\"][\"{key}\"]")));
        assert!(guard.contains("sqlx::query_scalar"));
        assert!(
            !guard.contains(
                "'INSERT') AND has_table_privilege(current_user,to_regclass(name),'UPDATE'"
            )
        );
        if key == "gateway_a2a" {
            for column in ["remote_task_id", "cancel_state"] {
                assert!(guard.contains(&format!("has_column_privilege(current_user,'gateway.gateway_a2a_outbound_effects','{column}','UPDATE')")));
            }
            for column in ["remote_message_id", "card_sha256", "card_key_id"] {
                assert!(guard.contains(&format!("NOT has_column_privilege(current_user,'gateway.gateway_a2a_outbound_effects','{column}','UPDATE')")));
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires approved loopback schema_successor_verify_* clone with030 plus outbound-authority schema/grants and owner-capable migrator; ACL changes always roll back"]
async fn every_required_column_and_forbidden_table_identity_privilege_is_effective() {
    assert_eq!(
        std::env::var("AI_PLATFORM_ISOLATED_TEST_FIXTURE").as_deref(),
        Ok("1")
    );
    let database = std::env::var("AUTHORITY_STARTUP_ACL_TEST_DATABASE")
        .expect("explicit isolated database required");
    measure_successors::validate_database_name(&database).unwrap();
    let dsn = credentials::dsn_for_principal("migrator")
        .expect("private migrator configuration required");
    let url = url::Url::parse(&dsn).expect("private migrator endpoint invalid");
    assert!(
        url.host_str()
            .and_then(|s| s.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
    );
    assert_eq!(url.path(), format!("/{database}"));
    let prefix = credentials::role_prefix_from_env().unwrap();
    let mut conn = credentials::connect_principal("migrator").await.unwrap();
    let identity: (String, String) =
        sqlx::query_as("SELECT current_database()::text,current_user::text")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(identity, (database, format!("{prefix}migrator")));
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("SELECT set_config('role',$1,true)")
        .bind(format!("{prefix}owner"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let catalog = contracts();
    for (key, suffix, tables, mut mutable, mut immutable) in [
        (
            "gateway_a2a",
            "gateway",
            vec![
                "gateway.gateway_a2a_contexts",
                "gateway.gateway_a2a_tasks",
                "gateway.gateway_a2a_outbound_effects",
            ],
            vec![
                ("gateway.gateway_a2a_contexts", "created_at"),
                ("gateway.gateway_a2a_tasks", "effect_state"),
                ("gateway.gateway_a2a_tasks", "run_id"),
                ("gateway.gateway_a2a_tasks", "thread_id"),
                ("gateway.gateway_a2a_tasks", "updated_at"),
                ("gateway.gateway_a2a_outbound_effects", "effect_state"),
                ("gateway.gateway_a2a_outbound_effects", "response"),
                ("gateway.gateway_a2a_outbound_effects", "updated_at"),
            ],
            vec![
                ("gateway.gateway_a2a_contexts", "tenant_id"),
                ("gateway.gateway_a2a_contexts", "user_id"),
                ("gateway.gateway_a2a_contexts", "publication_id"),
                ("gateway.gateway_a2a_contexts", "context_id"),
                ("gateway.gateway_a2a_contexts", "version_id"),
                ("gateway.gateway_a2a_tasks", "tenant_id"),
                ("gateway.gateway_a2a_tasks", "user_id"),
                ("gateway.gateway_a2a_tasks", "publication_id"),
                ("gateway.gateway_a2a_tasks", "context_id"),
                ("gateway.gateway_a2a_tasks", "task_id"),
                ("gateway.gateway_a2a_tasks", "message_id"),
                ("gateway.gateway_a2a_tasks", "request_hash"),
                ("gateway.gateway_a2a_outbound_effects", "tenant_id"),
                ("gateway.gateway_a2a_outbound_effects", "user_id"),
                ("gateway.gateway_a2a_outbound_effects", "agent_name"),
                ("gateway.gateway_a2a_outbound_effects", "message_id"),
                ("gateway.gateway_a2a_outbound_effects", "request_hash"),
            ],
        ),
        (
            "knowledge_worker",
            "knowledge_worker",
            vec!["knowledge.pipeline_work"],
            vec![
                ("knowledge.pipeline_work", "job_id"),
                ("knowledge.pipeline_work", "phase"),
                ("knowledge.pipeline_work", "status"),
                ("knowledge.pipeline_work", "execution_id"),
                ("knowledge.pipeline_work", "checkpoints"),
                ("knowledge.pipeline_work", "owner_token"),
                ("knowledge.pipeline_work", "lease_expires_at"),
                ("knowledge.pipeline_work", "fence"),
                ("knowledge.pipeline_work", "updated_at"),
            ],
            vec![
                ("knowledge.pipeline_work", "tenant_id"),
                ("knowledge.pipeline_work", "dataset_id"),
                ("knowledge.pipeline_work", "document_id"),
            ],
        ),
    ] {
        let role = format!("{prefix}{suffix}");
        let mut sql = catalog["admission_requirements"][key]
            .as_str()
            .unwrap()
            .replace("current_user", &format!("'{role}'"));
        if key == "gateway_a2a" {
            // Exercise the exact supplemental query at the real call site too:
            // outbound's later schema adds two mutable and three immutable fields.
            let guard = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../platform-gateway/src/migration_host/control_plane/a2a.rs"
            ));
            let extra = guard
                .split_once("\"SELECT COUNT(*)=5 AND ")
                .unwrap()
                .1
                .split_once('"')
                .unwrap()
                .0;
            let extra = format!("SELECT COUNT(*)=5 AND {extra}")
                .replace("current_user", &format!("'{role}'"));
            sql = format!("SELECT ({sql}) AND ({extra})");
            for column in ["remote_task_id", "cancel_state"] {
                mutable.push(("gateway.gateway_a2a_outbound_effects", column));
            }
            for column in ["remote_message_id", "card_sha256", "card_key_id"] {
                immutable.push(("gateway.gateway_a2a_outbound_effects", column));
            }
        }
        assert!(
            sqlx::query_scalar::<_, bool>(&sql)
                .fetch_one(&mut *tx)
                .await
                .unwrap(),
            "measured minimal role must pass {key}"
        );
        let mut scenarios = Vec::new();
        for table in tables {
            for privilege in ["SELECT", "INSERT"] {
                scenarios.push((
                    format!("REVOKE {privilege} ON {table} FROM {role}"),
                    format!("GRANT {privilege} ON {table} TO {role}"),
                ));
            }
            for privilege in ["UPDATE", "DELETE"] {
                scenarios.push((
                    format!("GRANT {privilege} ON {table} TO {role}"),
                    format!("REVOKE {privilege} ON {table} FROM {role}"),
                ));
            }
        }
        for (table, column) in mutable {
            scenarios.push((
                format!("REVOKE UPDATE ({column}) ON {table} FROM {role}"),
                format!("GRANT UPDATE ({column}) ON {table} TO {role}"),
            ));
        }
        for (table, column) in immutable {
            scenarios.push((
                format!("GRANT UPDATE ({column}) ON {table} TO {role}"),
                format!("REVOKE UPDATE ({column}) ON {table} FROM {role}"),
            ));
        }
        for (change, restore) in scenarios {
            tx.execute(change.as_str()).await.unwrap();
            assert!(
                !sqlx::query_scalar::<_, bool>(&sql)
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap(),
                "guard missed independent ACL change: {change}"
            );
            tx.execute(restore.as_str()).await.unwrap();
            assert!(
                sqlx::query_scalar::<_, bool>(&sql)
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap(),
                "minimal role must recover after rollback of one permission"
            );
        }
    }
    tx.rollback().await.unwrap();
    conn.close().await.unwrap();
}
