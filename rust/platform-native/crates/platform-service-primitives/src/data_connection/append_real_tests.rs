//! Requires an explicitly owned restored copy with admitted partition successor,
//! two prepared clock periods, exact app-role mTLS inputs and invoker grants.
//! No DDL, no commits, no production-name fallback, no fixture credentials.
use super::{
    ConnectionConfig, ConnectionPurpose, DatabaseRole, TenantPool, TenantScope,
    append::{self, AppendMode},
};
use serde_json::json;
#[tokio::test]
#[ignore = "explicit isolated restored-copy authority/partition/role rehearsal required"]
async fn real_global_usage_identity_and_audit_id_survive_cross_period_replay() {
    assert_eq!(
        std::env::var("APPEND_ACCEPTANCE_AUTHORIZED").as_deref(),
        Ok("isolated-copy-only")
    );
    let environment = std::env::vars().collect();
    let config = ConnectionConfig::from_environment(
        "APPEND_ACCEPTANCE",
        DatabaseRole::Gateway,
        ConnectionPurpose::TenantTransactions,
        &environment,
    )
    .unwrap();
    let pool = TenantPool::connect(&config).await.unwrap();
    let scope = TenantScope::new(&std::env::var("APPEND_ACCEPTANCE_TENANT").unwrap()).unwrap();
    let first = std::env::var("APPEND_ACCEPTANCE_FIRST_TIME").unwrap();
    let second = std::env::var("APPEND_ACCEPTANCE_SECOND_TIME").unwrap();
    let a: chrono::DateTime<chrono::Utc> = first.parse().unwrap();
    let b: chrono::DateTime<chrono::Utc> = second.parse().unwrap();
    use chrono::Datelike;
    assert_ne!((a.year(), a.month()), (b.year(), b.month()));
    let mut tx = pool.begin(&scope).await.unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(tx.connection())
        .await
        .unwrap();
    assert!(database.starts_with("af_partition_acceptance_"));
    let request = format!("partition-fixture-{}", uuid::Uuid::new_v4());
    let mut row = json!({"tenant_id":scope.as_str(),"user_id":"isolated-partition-fixture","request_id":request,"model":"partition-fixture","provider":"isolated-test","input_tokens":5,"output_tokens":1,"created_at":first});
    let first_write = append::execute(
        &mut tx,
        "gateway.usage_records",
        &row,
        AppendMode::UsageMerge,
        Some("idx_usage_records_request_identity"),
    )
    .await
    .unwrap();
    assert!(first_write.newly_inserted());
    row["created_at"] = json!(second);
    row["input_tokens"] = json!(7);
    row["service_id"] = json!("");
    row["request_type"] = json!("");
    let replay = append::execute(
        &mut tx,
        "gateway.usage_records",
        &row,
        AppendMode::UsageMerge,
        Some("idx_usage_records_request_identity"),
    )
    .await
    .unwrap();
    assert!(!replay.newly_inserted());
    assert_eq!(replay.outcome, "merged");
    assert_eq!(replay.row.unwrap()["input_tokens"], 7);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM gateway.usage_records WHERE tenant_id=$1 AND request_id=$2",
    )
    .bind(scope.as_str())
    .bind(&request)
    .fetch_one(tx.connection())
    .await
    .unwrap();
    assert_eq!(count, 1);
    let audit = json!({"tenant_id":scope.as_str(),"event_type":"partition_fixture","action":"test","status":"success","created_at":first});
    let inserted = append::execute(
        &mut tx,
        "gateway.audit_logs",
        &audit,
        AppendMode::Insert,
        None,
    )
    .await
    .unwrap();
    let id = inserted.row.unwrap()["id"].clone();
    let replay = json!({"id":id,"tenant_id":scope.as_str(),"event_type":"partition_fixture","action":"test","status":"success","created_at":second});
    let result = append::execute(
        &mut tx,
        "gateway.audit_logs",
        &replay,
        AppendMode::Ignore,
        None,
    )
    .await
    .unwrap();
    assert!(!result.inserted);
    let foreign = json!({"tenant_id":"different-tenant","request_id":request});
    assert!(
        append::execute(
            &mut tx,
            "gateway.usage_records",
            &foreign,
            AppendMode::Insert,
            None
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    pool.close().await;
}

/// Actual billing receipt SQL used by UsageStageReceipts. Unknown policy must
/// abort before mutation; a duplicate never manufactures a stage receipt row.
#[tokio::test]
#[ignore = "explicit isolated restored-copy authority/partition/role rehearsal required"]
async fn actual_receipt_query_preserves_returning_and_rejects_unapproved_policy() {
    assert_eq!(
        std::env::var("APPEND_ACCEPTANCE_AUTHORIZED").as_deref(),
        Ok("isolated-copy-only")
    );
    let config = ConnectionConfig::from_environment(
        "APPEND_ACCEPTANCE",
        DatabaseRole::Gateway,
        ConnectionPurpose::TenantTransactions,
        &std::env::vars().collect(),
    )
    .unwrap();
    let pool = TenantPool::connect(&config).await.unwrap();
    let scope = TenantScope::new(&std::env::var("APPEND_ACCEPTANCE_TENANT").unwrap()).unwrap();
    let mut tx = pool.begin(&scope).await.unwrap();
    let db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(tx.connection())
        .await
        .unwrap();
    assert!(db.starts_with("af_partition_acceptance_"));
    assert!(
        super::append_store::enabled(tx.connection(), "gateway.billing_events")
            .await
            .unwrap()
    );
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("append_store/native_statements.json")).unwrap();
    let sql = cases
        .iter()
        .find(|v| {
            v["family"] == "gateway.billing_events"
                && v["statement_tail"].as_str().unwrap().contains("RETURNING")
        })
        .unwrap()["sql"]
        .as_str()
        .unwrap();
    let id = format!("append-real-{}", uuid::Uuid::new_v4());
    for expected in [true, false] {
        let query = sqlx::query(sql)
            .bind(&id)
            .bind(&id)
            .bind("append-fixture-service")
            .bind("append-fixture-user")
            .bind(scope.as_str())
            .bind("append-fixture-model")
            .bind("append-fixture-assistant")
            .bind(json!({"event":"isolated-only"}))
            .bind(chrono::Utc::now());
        let result: Option<String> = super::append_store::fetch_scalar_optional(
            tx.connection(),
            "gateway.billing_events",
            query,
        )
        .await
        .unwrap();
        assert_eq!(result.is_some(), expected);
    }
    // An existing conflict must still run the original INSERT NOT NULL check.
    {
        let mut invalid = sqlx::Connection::begin(tx.connection()).await.unwrap();
        let query = sqlx::query(sql)
            .bind(&id)
            .bind(&id)
            .bind("append-fixture-service")
            .bind("append-fixture-user")
            .bind(scope.as_str())
            .bind("append-fixture-model")
            .bind("append-fixture-assistant")
            .bind(Option::<serde_json::Value>::None)
            .bind(chrono::Utc::now());
        let error =
            super::append_store::fetch_optional(&mut invalid, "gateway.billing_events", query)
                .await
                .err()
                .expect("original NOT NULL check must run before duplicate handling");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23502")
        );
        invalid.rollback().await.unwrap();
    }
    let unknown = sql.replace(
        "RETURNING event_id",
        "RETURNING event_id AS unadmitted_change",
    );
    let query = sqlx::query(&unknown)
        .bind(format!("unapproved-{}", uuid::Uuid::new_v4()))
        .bind(&id)
        .bind("append-fixture-service")
        .bind("append-fixture-user")
        .bind(scope.as_str())
        .bind("append-fixture-model")
        .bind("append-fixture-assistant")
        .bind(json!({}))
        .bind(chrono::Utc::now());
    assert!(
        super::append_store::fetch_optional(tx.connection(), "gateway.billing_events", query)
            .await
            .is_err()
    );
    // The failed statement leaves the original transaction aborted; no SQL
    // fallback or partial accounting commit is possible.
    assert!(
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(tx.connection())
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    pool.close().await;
}
