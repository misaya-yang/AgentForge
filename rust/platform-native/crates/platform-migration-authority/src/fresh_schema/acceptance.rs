//! Actual, bounded development-only acceptance using private runtime connections.
//! No credentials enter the compiler role. Every negative DDL/GRANT rolls back.
use super::*;
use sqlx::{Connection, PgConnection, Row};
use std::{collections::BTreeSet, time::Duration};

pub async fn acceptance(expected: &str) -> Result<Value> {
    install::target(expected)?;
    execution::stage(
        "fresh.actual-acceptance",
        Duration::from_secs(120),
        inner(expected),
    )
    .await
}
async fn inner(expected: &str) -> Result<Value> {
    let before = verify(expected).await?;
    let business = super::business::actual().await?;
    let prefix = crate::credentials::role_prefix_from_env()?;
    let config = crate::credentials::connection_for_principal("migrator")?;
    if config.tls_key_identity.is_none() {
        return Err(refused(
            "actual acceptance requires the private mTLS migrator",
        ));
    }
    let mut conn = PgConnection::connect_with(&config.options)
        .await
        .map_err(|_| refused("actual acceptance connection failed"))?;
    install::login(&mut conn, &prefix, "migrator", true).await?;
    let outcome = exercise(&mut conn, &config.options, &prefix).await;
    execution::discard(conn).await;
    let mut evidence = outcome?;
    let after = verify(expected).await?;
    if before["fingerprints"] != after["fingerprints"] {
        return Err(refused("acceptance changed the final schema"));
    }
    evidence["final_verification"] = after;
    evidence["gateway_business_rollback"] = business;
    Ok(evidence)
}
async fn exercise(
    observer: &mut PgConnection,
    options: &sqlx::postgres::PgConnectOptions,
    prefix: &str,
) -> Result<Value> {
    let owner = format!("{prefix}owner");
    let mut negatives = 0;
    for ddl in [
        "ALTER TABLE gateway.gateway_a2a_tasks ADD COLUMN fresh_guard_probe text".to_owned(),
        "CREATE INDEX fresh_guard_probe_idx ON gateway.gateway_a2a_tasks(created_at)".to_owned(),
        "ALTER TABLE gateway.gateway_a2a_tasks ADD CONSTRAINT fresh_guard_probe_check CHECK(true)"
            .to_owned(),
        format!("GRANT UPDATE ON gateway.gateway_a2a_outbound_effects TO \"{prefix}gateway\""),
        "CREATE POLICY fresh_guard_probe_policy ON knowledge.pipeline_work USING(false)".to_owned(),
        "ALTER FUNCTION assistant.claim_runtime_thread_owner(uuid,uuid,text) SECURITY INVOKER"
            .to_owned(),
    ] {
        let mut tx = observer.begin().await?;
        crate::runner::set_role(&mut tx, &owner).await?;
        execution::execute(&mut tx, &ddl).await?;
        let snapshot: Value =
            sqlx::query_scalar("SELECT gateway.platform_fresh_schema_snapshot_v1()")
                .fetch_one(&mut *tx)
                .await?;
        if snapshot["catalog_matches"] != false || validate_snapshot(&snapshot).is_ok() {
            return Err(refused("actual structural/ACL/RLS drift was accepted"));
        }
        tx.rollback().await?;
        negatives += 1;
    }
    let definitions = source::compiled_definitions()?;
    let grants = crate::bootstrap::render_role_sql(&definitions.baseline_sql[2], prefix)?;
    let verify_sql = crate::bootstrap::render_role_sql(&definitions.verify_sql, prefix)?;
    let mut tx = observer.begin().await?;
    crate::runner::set_role(&mut tx, &owner).await?;
    execution::stage(
        "acceptance.complete-grant-stream",
        Duration::from_secs(60),
        execution::execute(&mut tx, &grants),
    )
    .await?;
    let rows = execution::stage(
        "acceptance.all-followup-checks",
        execution::SQL_LIMIT,
        async { Ok(sqlx::raw_sql(&verify_sql).fetch_all(&mut *tx).await?) },
    )
    .await?;
    let mut names = BTreeSet::new();
    for row in &rows {
        names.insert(row.try_get::<String, _>("check_name")?);
        row.try_get::<bool, _>("ok")?;
    }
    if rows.len() != 251 || names.len() != 251 || !names.contains("grant-matrix") {
        return Err(refused("incomplete actual baseline verification transport"));
    }
    tx.rollback().await?;
    // The historical baseline verdict is not modern schema acceptance. Restore
    // all grants before final current-source verification below.
    for active in [false, true] {
        let mut conn = PgConnection::connect_with(options)
            .await
            .map_err(|_| refused("cancellation regression connection failed"))?;
        install::login(&mut conn, prefix, "migrator", true).await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut conn)
            .await?;
        let mut tx = conn.begin().await?;
        crate::runner::set_role(&mut tx, &owner).await?;
        execution::execute(&mut tx,"SET LOCAL statement_timeout='500ms';SET LOCAL idle_in_transaction_session_timeout='2s';SET LOCAL client_connection_check_interval='1s';CREATE TABLE public.fresh_cancel_owned_probe(id integer)").await?;
        let cancelled = if active {
            tokio::time::timeout(
                Duration::from_millis(20),
                sqlx::raw_sql("SELECT pg_sleep(30)").fetch_all(&mut *tx),
            )
            .await
            .is_err()
        } else {
            tokio::time::timeout(Duration::from_millis(20), std::future::pending::<()>())
                .await
                .is_err()
        };
        if !cancelled {
            return Err(refused("actual cancellation deadline did not fire"));
        }
        drop(tx);
        execution::discard(conn).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let gone: bool = sqlx::query_scalar(
                    "SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
                )
                .bind(pid)
                .fetch_one(&mut *observer)
                .await?;
                if gone {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok::<(), sqlx::Error>(())
        })
        .await
        .map_err(|_| refused("cancelled owned backend remains present"))??;
        let absent: bool =
            sqlx::query_scalar("SELECT to_regclass('public.fresh_cancel_owned_probe') IS NULL")
                .fetch_one(&mut *observer)
                .await?;
        if !absent {
            return Err(refused("cancelled owned DDL did not roll back"));
        }
    }
    Ok(
        json!({"status":"passed_actual_fresh_schema_acceptance","database":DATABASE,
        "source_contract_sha256":source_sha256(),"catalog_drift_negatives":negatives,
        "complete_grant_statements":execution::statements(&grants)?.len(),"followup_checks":rows.len(),
        "actual_mtls":true,"idle_and_active_backend_cancellation":true,"cancelled_ddl_rolled_back":true,
        "historical_upgrade_acceptance":false,"all_probe_transactions_rolled_back":true}),
    )
}
