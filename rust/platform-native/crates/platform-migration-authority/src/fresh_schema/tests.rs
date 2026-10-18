use super::*;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
#[test]
fn fresh_source_contains_every_definition_and_all_22_data_families_without_upgrade_proofs() {
    let root = root();
    let source = source_contract().unwrap();
    let defs = source::definitions(&root).unwrap();
    let compiled = source::compiled_definitions().unwrap();
    assert_eq!(
        defs.changes
            .iter()
            .map(|(s, sql)| (s.sha256.as_str(), sql))
            .collect::<Vec<_>>(),
        compiled
            .changes
            .iter()
            .map(|(s, sql)| (s.sha256.as_str(), sql))
            .collect::<Vec<_>>()
    );
    assert_eq!(defs.changes.len(), 35);
    assert!(
        defs.changes
            .iter()
            .all(|(s, sql)| s.sha256 == sha(sql.as_bytes()))
    );
    let all = defs
        .changes
        .iter()
        .map(|(_, s)| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for required in [
        "assistant.assistant_runtime_thread_owners",
        "gateway.authorization_resource_decisions",
        "gateway.gateway_a2a_tasks",
        "knowledge.pipeline_work",
        "partition_ops.families",
        "remote_task_id",
        "cancel_state",
    ] {
        assert!(all.contains(required), "missing {required}");
    }
    let families: Vec<String> = serde_json::from_value(source["families"].clone()).unwrap();
    assert_eq!(families.iter().collect::<BTreeSet<_>>().len(), 22);
    for family in families {
        assert!(all.contains(&format!("'{family}'")), "missing {family}");
    }
    // Only database-local schema/ACL preparation is taken from roles.sql.
    assert!(!defs.owner_setup.contains("ALTER ROLE"));
    assert!(!defs.owner_setup.contains("CREATE ROLE"));
    assert!(defs.owner_setup.contains("ALTER DEFAULT PRIVILEGES"));
    assert_eq!(defs.baseline_sql.len(), 3);
    let inputs = source["inputs_sha256"].as_object().unwrap();
    assert!(!inputs.keys().any(|p| p.contains("verification.json")
        || p.contains("MEASUREMENT")
        || p.contains("ACTIVATION")
        || p.contains("native-producer")));
    assert_eq!(source_inventory(&root).unwrap()["database_actions"], false);
}
#[test]
fn complete_fresh_catalog_covers_all_fields_indexes_constraints_roles_and_rls() {
    let sql = catalog::catalog_sql("deployment_").unwrap();
    let q: Value =
        serde_json::from_str(include_str!("../fingerprint/catalog_queries.json")).unwrap();
    for key in q.as_object().unwrap().keys() {
        assert!(sql.contains(&catalog::literal(key)), "missing {key}");
    }
    assert!(sql.contains("partition_ops"));
    assert!(sql.contains("pg_get_functiondef"));
    for key in [
        "extension/member_acl",
        "extension/type_members",
        "extension/operator_members",
        "extension/opclass_members",
        "extension/index_operators",
        "extension/index_support",
    ] {
        assert!(sql.contains(key), "missing executable/index closure {key}");
    }

    assert!(sql.contains("policy_acl_lines/rows"));
    assert!(sql.contains("column_acl_lines/rows"));
    assert!(sql.contains("reference/permissions"));
    assert!(sql.contains("authority/attempts"));
    assert!(!sql.contains("$1"));
    assert!(!sql.contains("$2"));
    assert!(catalog::catalog_sql("bad';DROP ROLE owner;--_").is_err());
    let installed = catalog::install_sql("deployment_").unwrap();
    assert!(installed.contains("REVOKE ALL ON public.platform_fresh_schema_state FROM PUBLIC"));
    assert!(installed.contains("SECURITY DEFINER SET search_path=pg_catalog,pg_temp"));
    assert!(!installed.contains("TO PUBLIC"));
}
#[test]
fn fresh_admission_refuses_every_missing_forged_or_historical_field() {
    let mut value = admission_contract();
    value["fingerprints"] = json!({"structural":"a".repeat(64),"acl":"b".repeat(64),"extensions":"c".repeat(64),"reference_data":"d".repeat(64)});
    validate_snapshot(&value).unwrap();
    for key in value.as_object().unwrap().keys() {
        let mut changed = value.clone();
        changed.as_object_mut().unwrap().remove(key);
        assert!(
            validate_snapshot(&changed).is_err(),
            "accepted missing {key}"
        );
    }
    for (key, bad) in [
        ("catalog_matches", json!(false)),
        ("database", json!("gateway")),
        ("schema_epoch", json!(29)),
        ("source_contract_sha256", json!("0".repeat(64))),
        ("historical_upgrade_acceptance", json!(true)),
        ("mode", json!("upgrade")),
    ] {
        let mut changed = value.clone();
        changed[key] = bad;
        assert!(validate_snapshot(&changed).is_err(), "accepted {key}");
    }
    let mut extra = value.clone();
    extra["unreviewed"] = true.into();
    assert!(validate_snapshot(&extra).is_err());
    for key in ["acl", "structural", "extensions", "reference_data"] {
        let mut changed = value.clone();
        changed["fingerprints"][key] = json!("not measured");
        assert!(validate_snapshot(&changed).is_err());
    }
}
#[test]
fn complete_fresh_acl_refuses_missing_duplicate_column_or_broad_privileges() {
    let policy: Value = serde_json::from_str(include_str!("acl_expected.json")).unwrap();
    let exact: Vec<String> = serde_json::from_value(policy["raw_acl_lines"].clone()).unwrap();
    install::exact_acl(&exact).unwrap();
    for line in [
        "column_acl:gateway.gateway_a2a_outbound_effects.remote_task_id:gateway:UPDATE:grantopt=0",
        "column_acl:gateway.gateway_a2a_outbound_effects.cancel_state:gateway:UPDATE:grantopt=0",
    ] {
        assert!(exact.iter().any(|s| s == line));
    }
    for new in [
        "acl:gateway.gateway_a2a_tasks:gateway:UPDATE:grantopt=0",
        "acl:gateway.gateway_a2a_outbound_effects:gateway:UPDATE:grantopt=0",
        "acl:knowledge.pipeline_work:knowledge_worker:UPDATE:grantopt=0",
        "acl:knowledge.pipeline_work:PUBLIC:SELECT:grantopt=0",
    ] {
        let mut bad = exact.clone();
        bad.push(new.into());
        assert!(install::exact_acl(&bad).is_err());
    }
    let mut missing = exact.clone();
    missing.pop();
    assert!(install::exact_acl(&missing).is_err());
    let mut duplicate = exact.clone();
    duplicate.push(exact[0].clone());
    assert!(install::exact_acl(&duplicate).is_err());
}
#[tokio::test]
async fn unrelated_or_existing_database_names_fail_before_source_or_connection_access() {
    for database in [
        "gateway",
        "postgres",
        "schema_successor_verify_test",
        "gateway_dev_fresh ",
        "",
    ] {
        assert!(initialize(database).await.is_err());
        assert!(verify(database).await.is_err());
    }
}

/// Real SQL, owner DDL inside rolled-back transactions, against only the
/// explicitly initialized dedicated development database. Never a fixture pass.
#[tokio::test]
#[ignore = "requires actual initialized gateway_dev_fresh and six existing principal credentials; root owns PG execution"]
async fn actual_fresh_catalog_refuses_column_index_constraint_acl_rls_and_function_drift() {
    use sqlx::Connection;
    assert_eq!(
        std::env::var("AI_PLATFORM_FRESH_SCHEMA_VERIFY").as_deref(),
        Ok("1")
    );
    let root = root();
    verify(DATABASE).await.unwrap();
    let prefix = crate::credentials::role_prefix_from_env().unwrap();
    let authority = crate::runner::MigrationAuthority::from_principal(
        "migrator",
        crate::runner::AuthorityPaths {
            database_dir: root.join("database"),
        },
        prefix.clone(),
    )
    .unwrap();
    let mut conn = authority.connect(false).await.unwrap();
    for ddl in [
        "ALTER TABLE gateway.gateway_a2a_tasks ADD COLUMN fresh_guard_probe text".to_owned(),
        "CREATE INDEX fresh_guard_probe_idx ON gateway.gateway_a2a_tasks(created_at)".to_owned(),
        "ALTER TABLE gateway.gateway_a2a_tasks ADD CONSTRAINT fresh_guard_probe_check CHECK(true)"
            .to_owned(),
        format!("GRANT UPDATE ON gateway.gateway_a2a_tasks TO \"{prefix}gateway\""),
        "CREATE POLICY fresh_guard_probe_policy ON knowledge.pipeline_work USING(false)".to_owned(),
        "ALTER FUNCTION assistant.claim_runtime_thread_owner(uuid,uuid,text) SECURITY INVOKER"
            .to_owned(),
    ] {
        let mut tx = conn.begin().await.unwrap();
        crate::runner::set_role(&mut tx, &format!("{prefix}owner"))
            .await
            .unwrap();
        sqlx::raw_sql(&ddl).execute(&mut *tx).await.unwrap();
        let snapshot: Value =
            sqlx::query_scalar("SELECT gateway.platform_fresh_schema_snapshot_v1()")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(snapshot["catalog_matches"], false, "{ddl}");
        assert!(validate_snapshot(&snapshot).is_err(), "{ddl}");
        tx.rollback().await.unwrap();
    }
    conn.close().await.unwrap();
    verify(DATABASE).await.unwrap();
}

#[tokio::test]
#[ignore = "real file/mTLS protocol regression; requires initialized gateway_dev_fresh; all GRANT changes roll back"]
async fn actual_mtls_complete_grants_are_drained_before_all_251_followup_checks() {
    use sqlx::{Connection, Row};
    assert_eq!(
        std::env::var("AI_PLATFORM_FRESH_SCHEMA_VERIFY").as_deref(),
        Ok("1")
    );
    verify(DATABASE).await.unwrap();
    let prefix = crate::credentials::role_prefix_from_env().unwrap();
    let config = crate::credentials::connection_for_principal("migrator").unwrap();
    assert!(
        config.tls_key_identity.is_some(),
        "this regression must exercise actual mTLS"
    );
    let mut conn = sqlx::PgConnection::connect_with(&config.options)
        .await
        .unwrap();
    let defs = source::compiled_definitions().unwrap();
    let mut tx = conn.begin().await.unwrap();
    crate::runner::set_role(&mut tx, &format!("{prefix}owner"))
        .await
        .unwrap();
    let grants = crate::bootstrap::render_role_sql(&defs.baseline_sql[2], &prefix).unwrap();
    execution::stage(
        "test.complete-grants",
        std::time::Duration::from_secs(60),
        execution::execute(&mut tx, &grants),
    )
    .await
    .unwrap();
    let query = crate::bootstrap::render_role_sql(&defs.verify_sql, &prefix).unwrap();
    let rows = execution::stage("test.followup-all-checks", execution::SQL_LIMIT, async {
        Ok(sqlx::raw_sql(&query).fetch_all(&mut *tx).await?)
    })
    .await
    .unwrap();
    assert_eq!(rows.len(), 251);
    let names = rows
        .iter()
        .map(|r| r.try_get::<String, _>("check_name").unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), 251);
    assert!(names.contains("grant-matrix"));
    for row in rows {
        row.try_get::<bool, _>("ok").unwrap();
    }
    // A current035 database has successor ACL additions: the historical baseline
    // verdict is not fresh acceptance. This test asserts full protocol delivery;
    // rollback restores every current grant, then real final admission must pass.
    tx.rollback().await.unwrap();
    execution::discard(conn).await;
    verify(DATABASE).await.unwrap();
}

#[tokio::test]
#[ignore = "real cancellation/rollback regression; requires initialized gateway_dev_fresh and dedicated migrator"]
async fn actual_cancelled_idle_transaction_discards_backend_and_rolls_back_ddl() {
    use sqlx::Connection;
    use std::time::Duration;
    assert_eq!(
        std::env::var("AI_PLATFORM_FRESH_SCHEMA_VERIFY").as_deref(),
        Ok("1")
    );
    let prefix = crate::credentials::role_prefix_from_env().unwrap();
    let config = crate::credentials::connection_for_principal("migrator").unwrap();
    let mut conn = sqlx::PgConnection::connect_with(&config.options)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    let mut tx = conn.begin().await.unwrap();
    crate::runner::set_role(&mut tx, &format!("{prefix}owner"))
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE public.fresh_cancel_owned_probe(id integer)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let result: Result<()> = execution::stage(
        "test.owned-idle-cancel",
        Duration::from_millis(20),
        std::future::pending(),
    )
    .await;
    assert!(result.is_err());
    drop(tx);
    execution::discard(conn).await;
    let mut observer = sqlx::PgConnection::connect_with(&config.options)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let gone: bool = sqlx::query_scalar(
                "SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
            )
            .bind(pid)
            .fetch_one(&mut observer)
            .await
            .unwrap();
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("owned cancelled backend must not remain idle in transaction");
    let absent: bool =
        sqlx::query_scalar("SELECT to_regclass('public.fresh_cancel_owned_probe') IS NULL")
            .fetch_one(&mut observer)
            .await
            .unwrap();
    assert!(absent, "uncommitted DDL must be rolled back");
    execution::discard(observer).await;
    verify(DATABASE).await.unwrap();
}

#[test]
fn exact_installed_acl_requires_all_private_metadata_lines_without_hiding_extras() {
    let p: Value = serde_json::from_str(include_str!("acl_expected.json")).unwrap();
    let mut acl: Vec<String> = serde_json::from_value(p["raw_acl_lines"].clone()).unwrap();
    let metadata = install::metadata_acl();
    assert_eq!(metadata.len(), 15);
    acl.extend(metadata.clone());
    install::exact_installed_acl(&acl).unwrap();
    for line in metadata {
        let mut bad = acl.clone();
        bad.retain(|s| s != &line);
        assert!(install::exact_installed_acl(&bad).is_err());
    }
    for extra in [
        "acl:public.platform_fresh_schema_state:gateway:SELECT:grantopt=0",
        "function_acl:gateway.platform_fresh_catalog_v1():gateway:EXECUTE:grantopt=0",
        "function_acl:assistant.platform_fresh_schema_snapshot_v1():PUBLIC:EXECUTE:grantopt=0",
    ] {
        let mut bad = acl.clone();
        bad.push(extra.into());
        assert!(install::exact_installed_acl(&bad).is_err());
    }
    let mut bad = acl.clone();
    bad.push(acl.last().unwrap().clone());
    assert!(install::exact_installed_acl(&bad).is_err());
}
