//! Explicit isolated-database acceptance; never connects in the ordinary unit suite.
use super::*;
#[tokio::test]
#[ignore = "requires separately authorized isolated af_data_* database, installed authority RLS proposal, role mTLS files and two seeded tenant datasets"]
async fn real_rls_and_transaction_pool_scope_do_not_leak_between_tenants() {
    let environment: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    assert_eq!(
        environment
            .get("DATA_ACCEPTANCE_AUTHORIZED")
            .map(String::as_str),
        Some("isolated-copy-only")
    );
    let mut config = ConnectionConfig::from_environment(
        "DATA_ACCEPTANCE",
        DatabaseRole::KnowledgeWorker,
        ConnectionPurpose::TenantTransactions,
        &environment,
    )
    .unwrap();
    assert!(config.physical_database.starts_with("af_data_"));
    config.max_connections = 1;
    let tenant_a = TenantScope::new(environment.get("DATA_ACCEPTANCE_TENANT_A").unwrap()).unwrap();
    let tenant_b = TenantScope::new(environment.get("DATA_ACCEPTANCE_TENANT_B").unwrap()).unwrap();
    assert_ne!(tenant_a, tenant_b);
    let a = environment.get("DATA_ACCEPTANCE_DATASET_A").unwrap();
    let b = environment.get("DATA_ACCEPTANCE_DATASET_B").unwrap();
    let pool = config.connect().await.unwrap();
    let unbound: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge.datasets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(unbound, 0);
    let mut empty = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_catalog.set_config('app.tenant_id','',true)")
        .execute(&mut *empty)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM knowledge.datasets")
        .fetch_one(&mut *empty)
        .await
        .unwrap();
    assert_eq!(count, 0);
    empty.rollback().await.unwrap();
    // Both fixtures must be visible under their actual owning scopes before negatives count.
    for (tenant, id) in [(&tenant_a, a), (&tenant_b, b)] {
        let mut tx = pool.begin().await.unwrap();
        bind_transaction(&mut tx, tenant, config.role, config.statement_timeout_ms)
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM knowledge.datasets WHERE dataset_id=$1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(count, 1);
        tx.rollback().await.unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    bind_transaction(&mut tx, &tenant_a, config.role, config.statement_timeout_ms)
        .await
        .unwrap();
    // Existing transaction identity is immutable even outside a request task.
    assert!(
        bind_transaction(&mut tx, &tenant_b, config.role, config.statement_timeout_ms)
            .await
            .is_err()
    );
    assert!(
        bind_transaction(
            &mut tx,
            &tenant_a,
            DatabaseRole::Runtime,
            config.statement_timeout_ms
        )
        .await
        .is_err()
    );
    let still_a: String = sqlx::query_scalar("SELECT current_setting('app.tenant_id')")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(still_a, tenant_a.as_str());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM knowledge.datasets WHERE dataset_id=$1")
            .bind(b)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(count, 0);
    // A positive same-row update proves this is not simply a missing UPDATE grant.
    assert_eq!(
        sqlx::query("UPDATE knowledge.datasets SET name=name WHERE dataset_id=$1")
            .bind(a)
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected(),
        1
    );
    sqlx::query("SAVEPOINT tenant_negative")
        .execute(&mut *tx)
        .await
        .unwrap();
    let error = sqlx::query("UPDATE knowledge.datasets SET tenant_id=$1 WHERE dataset_id=$2")
        .bind(tenant_b.as_str())
        .bind(a)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501")
    );
    sqlx::query("ROLLBACK TO SAVEPOINT tenant_negative")
        .execute(&mut *tx)
        .await
        .unwrap();
    let own_id = format!("rls-probe-{}", uuid::Uuid::new_v4());
    sqlx::query("INSERT INTO knowledge.datasets(dataset_id,name,tenant_id) VALUES($1,'transaction-rollback-probe',$2)").bind(&own_id).bind(tenant_a.as_str()).execute(&mut *tx).await.unwrap();
    sqlx::query("SAVEPOINT insert_negative")
        .execute(&mut *tx)
        .await
        .unwrap();
    let error = sqlx::query(
        "INSERT INTO knowledge.datasets(dataset_id,name,tenant_id) VALUES($1,'forbidden',$2)",
    )
    .bind(format!("foreign-{}", uuid::Uuid::new_v4()))
    .bind(tenant_b.as_str())
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501")
    );
    tx.rollback().await.unwrap();
    // The sole pool connection is reused: tenant state must not survive rollback or commit.
    let state: Option<String> =
        sqlx::query_scalar("SELECT NULLIF(pg_catalog.current_setting('app.tenant_id',true),'')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(state.is_none());
    let mut tx = pool.begin().await.unwrap();
    bind_transaction(&mut tx, &tenant_b, config.role, config.statement_timeout_ms)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let state: Option<String> =
        sqlx::query_scalar("SELECT NULLIF(pg_catalog.current_setting('app.tenant_id',true),'')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(state.is_none());
    let mut cancelled = pool.begin().await.unwrap();
    bind_transaction(
        &mut cancelled,
        &tenant_a,
        config.role,
        config.statement_timeout_ms,
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(10),
            sqlx::query("SELECT pg_sleep(0.2)").execute(&mut *cancelled)
        )
        .await
        .is_err()
    );
    drop(cancelled);
    let after_cancel: Option<String> =
        sqlx::query_scalar("SELECT NULLIF(current_setting('app.tenant_id',true),'')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(after_cancel.is_none());
    // Exercise the same executor used by production pool and pinned-lease calls.
    for (tenant, own, foreign) in [(&tenant_a, a, b), (&tenant_b, b, a)] {
        let visible: Vec<String> = sqlx::query_scalar(
            "SELECT dataset_id FROM knowledge.datasets WHERE dataset_id IN ($1,$2)",
        )
        .bind(own)
        .bind(foreign)
        .fetch_all(tenant_executor(
            &pool,
            tenant.as_str(),
            config.role,
            config.statement_timeout_ms,
        ))
        .await
        .unwrap();
        assert_eq!(visible, vec![own.clone()]);
        let state: Option<String> =
            sqlx::query_scalar("SELECT NULLIF(current_setting('app.tenant_id',true),'')")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(state.is_none());
    }
    {
        let mut connection = pool.acquire().await.unwrap();
        let scope = tenant_a.clone();
        let current: String = sqlx::query_scalar("SELECT current_setting('app.tenant_id')")
            .fetch_one(TenantExecutor::connection(
                &mut connection,
                config.role,
                config.statement_timeout_ms,
                move || Ok(Some(scope)),
            ))
            .await
            .unwrap();
        assert_eq!(current, tenant_a.as_str());
        let scope = tenant_a.clone();
        let failure = sqlx::query("SELECT 1/0")
            .execute(TenantExecutor::connection(
                &mut connection,
                config.role,
                config.statement_timeout_ms,
                move || Ok(Some(scope)),
            ))
            .await
            .unwrap_err();
        assert_eq!(
            failure
                .as_database_error()
                .and_then(|e| e.code())
                .as_deref(),
            Some("22012")
        );
        let state: Option<String> =
            sqlx::query_scalar("SELECT NULLIF(current_setting('app.tenant_id',true),'')")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert!(state.is_none());
    }
    pool.close().await;
}

#[tokio::test]
#[ignore = "explicit isolated restored-copy PgBouncer deployment and role/cert fixture; no business stack"]
async fn real_transaction_pool_replica_connections_and_prepared_statements_stay_bounded() {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    assert_eq!(
        env.get("DATA_ACCEPTANCE_AUTHORIZED").map(String::as_str),
        Some("isolated-copy-only")
    );
    let mut config = ConnectionConfig::from_environment(
        "DATA_ACCEPTANCE",
        DatabaseRole::KnowledgeWorker,
        ConnectionPurpose::TenantTransactions,
        &env,
    )
    .unwrap();
    assert!(config.physical_database.starts_with("af_data_"));
    assert_eq!(config.mode, ConnectionMode::Transaction);
    assert_eq!(config.pooled_backend_limit, 8);
    config.max_connections = 1;
    config.replicas = 16;
    config.validate().unwrap();
    let tenant = TenantScope::new(env.get("DATA_ACCEPTANCE_TENANT_A").unwrap()).unwrap();
    let mut pools = Vec::new();
    for _ in 0..16 {
        pools.push(config.connect().await.unwrap());
    }
    let mut jobs = tokio::task::JoinSet::new();
    for (n, pool) in pools.iter().cloned().enumerate() {
        let scope = tenant.clone();
        jobs.spawn(async move {
            let mut observed=Vec::new();
            // Same frontend/SQLx connection caches named statements across three
            // distinct transactions while sixteen clients share <=8 backends.
            for round in 0..3 {
                let mut tx=pool.begin().await.unwrap();
                bind_transaction(&mut tx,&scope,DatabaseRole::KnowledgeWorker,30000).await.unwrap();
                let value=format!("replica-{n}-round-{round}");
                let row:(String,String,i32,i64)=sqlx::query_as("SELECT $1::text,current_setting('app.tenant_id'),pg_backend_pid(),(SELECT count(*) FROM pg_stat_activity WHERE usename=current_user AND datname=current_database()) FROM pg_sleep(0.05)")
                    .bind(&value).fetch_one(&mut *tx).await.unwrap();
                assert_eq!(row.0,value);assert_eq!(row.1,scope.as_str());assert!(row.3<=8);
                tx.rollback().await.unwrap();observed.push(row.2);
            }
            observed
        });
    }
    let mut backends = std::collections::BTreeSet::new();
    while let Some(row) = jobs.join_next().await {
        backends.extend(row.unwrap());
    }
    assert!(!backends.is_empty() && backends.len() <= 8);
    for pool in pools {
        let scope: Option<String> =
            sqlx::query_scalar("SELECT NULLIF(current_setting('app.tenant_id',true),'')")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(scope.is_none());
        pool.close().await;
    }
}

#[tokio::test]
#[ignore = "explicit isolated RLS copy with two existing tenant document fixtures; reparent negatives roll back"]
async fn real_dataset_join_policy_rejects_cross_tenant_document_reparenting() {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    assert_eq!(
        env.get("DATA_ACCEPTANCE_AUTHORIZED").map(String::as_str),
        Some("isolated-copy-only")
    );
    let config = ConnectionConfig::from_environment(
        "DATA_ACCEPTANCE",
        DatabaseRole::KnowledgeWorker,
        ConnectionPurpose::TenantTransactions,
        &env,
    )
    .unwrap();
    assert!(config.physical_database.starts_with("af_data_"));
    let a = TenantScope::new(env.get("DATA_ACCEPTANCE_TENANT_A").unwrap()).unwrap();
    let b = TenantScope::new(env.get("DATA_ACCEPTANCE_TENANT_B").unwrap()).unwrap();
    assert_ne!(a, b);
    let doc_a = env.get("DATA_ACCEPTANCE_DOCUMENT_A").unwrap();
    let doc_b = env.get("DATA_ACCEPTANCE_DOCUMENT_B").unwrap();
    let dataset_b = env.get("DATA_ACCEPTANCE_DATASET_B").unwrap();
    let pool = config.connect().await.unwrap();
    for (tenant, id) in [(&a, doc_a), (&b, doc_b)] {
        let mut tx = pool.begin().await.unwrap();
        bind_transaction(&mut tx, tenant, config.role, 30000)
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM knowledge.documents WHERE document_id=$1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(count, 1);
        tx.rollback().await.unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    bind_transaction(&mut tx, &a, config.role, 30000)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM knowledge.documents WHERE document_id=$1")
            .bind(doc_b)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        sqlx::query("UPDATE knowledge.documents SET title=title WHERE document_id=$1")
            .bind(doc_a)
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected(),
        1
    );
    let error = sqlx::query("UPDATE knowledge.documents SET dataset_id=$1 WHERE document_id=$2")
        .bind(dataset_b)
        .bind(doc_a)
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("42501")
    );
    tx.rollback().await.unwrap();
    pool.close().await;
}
